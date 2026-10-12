<div align="center">

<img src="assets/brand/logo.svg" width="132" alt="Rescyo 标志：右上方一段正在归位的救生圈">

# Rescyo 数据救援

**免费开源、绝不写入源磁盘的数据恢复工具。**

[![CI](https://img.shields.io/github/actions/workflow/status/akk0sfx/Rescyo/ci.yml?branch=develop&label=CI&logo=github)](https://github.com/akk0sfx/Rescyo/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-GPL--3.0--or--later-2F6FEB)](LICENSE)
[![Status](https://img.shields.io/badge/status-alpha%20v0.3-FF5A1F)](docs/roadmap.md)
[![Platforms](https://img.shields.io/badge/platforms-Windows%207%E2%80%9311%20%C2%B7%20Linux%20%C2%B7%20macOS-3A3B4A)](#支持平台)
[![Rust](https://img.shields.io/badge/Rust-1.99-1B1B22?logo=rust)](rust-toolchain.toml)

[English](README.md) · [Русский](README.ru.md) · **简体中文** · [日本語](README.ja.md) · [한국어](README.ko.md)

[快速开始](#快速开始) · [为什么选择 Rescyo](#为什么选择-rescyo) · [工作原理](#工作原理) · [文件系统](#文件系统) · [路线图](docs/roadmap.md)

</div>

---

> 本译文为机器辅助翻译，等待母语者审校。欢迎提交改进。

Rescyo 可从损坏、已格式化或部分无法读取的硬盘、存储卡和 U 盘中找回文件。它能一次性恢复完整的文件夹树、不限文件数量；会告诉你每个文件是如何拼装的、内容校验是否通过；并会小心对待濒临损坏的磁盘。单个便携程序即可运行于 Windows 7–11、Linux 和 macOS，命令行输出稳定的 JSON。

DMDE、R-Studio 和 UFS Explorer 的免费开源替代品。

> [!IMPORTANT]
> **Alpha 版（v0.3）。** 命令行可恢复 FAT、exFAT 和 NTFS（包括压缩文件和备用数据流），能为故障磁盘制作带 ddrescue 兼容映射文件的镜像，并能找回丢失的分区；已通过逐字节恢复测试，但尚无签名的二进制文件。请始终恢复到另一块磁盘；磁盘出现故障时请先制作镜像。自 v0.3 起还能读取 VHD、VHDX、VMDK、qcow2 和 E01 镜像，按签名恢复文件，读取 ext4（实验性），并提供终端界面。

## 为什么选择 Rescyo

<table>
<tr>
<td width="50%" valign="top">

**从设计上只读**<br>
被恢复的磁盘仅以只读方式打开，并由类型系统保证：I/O 层没有写入操作。恢复的文件不能保存到同一物理设备——按设备检查，而不是按盘符。

</td>
<td width="50%" valign="top">

**如实标注每个文件**<br>
每个文件都标明其拼装方式（FAT 链、镜像 FAT、MFT 运行列表或“连续”推测）以及内容校验结果。满是垃圾数据的文件绝不会看起来和完好的文件一样。

</td>
</tr>
<tr>
<td valign="top">

**保护濒临损坏的介质**<br>
无法读取的扇区只读一次，记入坏扇区图，不再反复读取。磁盘状况恶化时，Rescyo 会暂停并建议先制作镜像。

</td>
<td valign="top">

**完整文件夹树，不设上限**<br>
一次恢复全部内容，包括父目录已损毁的文件夹（放入 `_LOST_FOLDERS`）。导入 DMDE 扫描日志，省去数小时的重新扫描。

</td>
</tr>
</table>

## 快速开始

尚未发布。从源码构建：

```sh
git clone https://github.com/akk0sfx/Rescyo.git
cd Rescyo
cargo build --release
```

命令行示例：

```sh
rescyo disks --json                              # 列出磁盘
rescyo scan /dev/sdb --session flash.db          # 查找卷和文件夹（只读）
rescyo image /dev/sdb --to /mnt/backup/flash.img  # 故障磁盘：先制作镜像（可续传）
rescyo partitions /dev/sdb                       # 查找分区表中缺失的分区
rescyo tui flash.db                              # 在终端中浏览和恢复
rescyo ls flash.db --deleted --ext jpg,docx      # 浏览找到的内容
rescyo recover flash.db --to /mnt/backup/flash   # 复制出来，并生成 SHA-256 报告
```

## 工作原理

```mermaid
flowchart LR
  S["源<br/>磁盘 · 镜像"] --> G["受保护读取<br/>只读 · 坏扇区图"]
  G --> E["引擎<br/>FAT · exFAT · NTFS"]
  E --> D[("会话<br/>SQLite")]
  D --> R["恢复<br/>到另一设备"]
  R --> P["报告<br/>JSON · CSV · SHA-256"]
```

## 文件系统

| 文件系统 | 支持深度 | 版本 |
|---|---|---|
| FAT12、FAT16、FAT32 | 完整：丢失的卷、文件夹树、已删除文件 | v0.1 |
| exFAT | 完整 | v0.1 |
| NTFS | 完整：MFT、已删除文件、稀疏文件；自 v0.2 起支持压缩（LZNT1）和 WOF 压缩（XPRESS、LZX）文件及备用数据流 | v0.1 |
| ext2、ext3、ext4 | 实验性 | v0.3 |
| HFS+、APFS、Btrfs | 实验性 | v0.4 |

## 支持平台

| 操作系统 | 架构 |
|---|---|
| Windows 10 / 11 | x64、arm64 |
| Windows 7 SP1 | x64、x86（独立构建） |
| Linux | x86_64、i686、aarch64、armv7、riscv64、loongarch64 |
| macOS 10.12+ | Apple 芯片、Intel |

## 许可证

GPL-3.0-or-later，见 [LICENSE](LICENSE)。本程序不提供任何担保。

本项目与 DMDE Software、R-Tools Technology 及 SysDev Laboratories 无任何关联；DMDE、R-Studio 和 UFS Explorer 是其各自所有者的商标。
