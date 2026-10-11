# Rescyo 命令行字符串。机器输出（JSON）不做本地化。待母语者审校。

app-about = 免费开源、绝不写入源磁盘的数据恢复工具。

disks-none = 未找到磁盘。读取磁盘可能需要管理员或 root 权限。
disks-size-unknown = 大小未知（需要管理员或 root 权限）
disks-removable = 可移动

scan-start = 正在扫描 { $source }…
phase-probe = 正在查找卷
phase-deep-scan = 正在读取整个源
phase-walk = 正在重建文件夹
phase-lost-folders = 正在放置丢失的文件夹
scan-volume = 卷 { $id }：{ $fs }，簇 { $cluster }，{ $files } 个文件，{ $dirs } 个文件夹，已删除 { $deleted } 个，丢失 { $lost } 个
scan-volume-skipped = 卷 { $id }：位于 { $offset } 的 { $fs }（评分 { $score }），未遍历
scan-no-volumes = 未找到卷。请尝试深度扫描：--deep。
scan-bad-sectors = 已跳过并记录 { $count } 个无法读取的扇区。
scan-done = 会话已保存：{ $session }

import-summary = DMDE 卷 { $index }：簇 { $cluster }，数据起始 { $offset }，日志中有 { $folders } 个文件夹。使用了 { $seeds } 个目录起点，{ $offgrid } 个因不在簇网格上而被舍弃。

ls-total = 共 { $count } 项

recover-plan = { $files } 个文件，{ $size }，目标：{ $target }
recover-done = 已恢复 { $files } 个文件（{ $size }）。报告：{ $report }
recover-partial = { $count } 个文件含有用零填充的无法读取部分，请查看报告。
recover-guessed = { $count } 个文件在没有分配信息的情况下按连续方式读取。使用前请先检查。
recover-resumed = { $count } 个文件已在上次运行中写入目标并通过校验。
recover-failed = { $count } 个文件无法写入，请查看报告。

info-source = 源：{ $path }
info-source-unverified = 会话所附加的源与其指纹不匹配。结果未经验证。
info-volumes = 卷：{ $count }
info-entries = 条目：{ $count }

problem-target-on-source-device = 目标与源位于同一块物理磁盘（{ $devices }）。请选择另一块磁盘上的文件夹。
problem-unverified-target = Rescyo 无法确定目标所在的磁盘（{ $reason }）。请选择其他文件夹；若确定是另一块磁盘，可使用 --allow-unverified-target。
problem-source-devices-unknown = Rescyo 无法确定源所在的磁盘（{ $reason }）。
problem-insufficient-space = 空间不足：需要 { $needed }，可用 { $free }。
problem-file-too-large = { $path } 大小为 { $size }，超过目标文件系统的上限（{ $limit }）。请使用 exFAT 或 NTFS 格式的目标。
problem-nothing-selected = 没有符合所选条件的项目。

error-source-open = 无法打开 { $source }：{ $reason }
error-source-mismatch = { $source } 不是创建此会话时使用的源（{ $reason }）。仅在确定时使用 --force-rebind。
error-session-exists = { $path } 已存在。Rescyo 绝不覆盖会话，请换一个文件名。
error-session-invalid = { $path } 不是 Rescyo 会话：{ $reason }
error-session-on-source = 会话文件会位于源磁盘上（{ $devices }）。请保存到另一块磁盘。
error-degraded = 源正在劣化：{ $count } 个扇区无法读取。请立即停止，先制作镜像（例如使用 GNU ddrescue），再扫描镜像。如仍要继续，请使用 --continue-on-errors。
error-gone = 源已断开：{ $reason }
error-dmde-parse = 无法读取 DMDE 日志：{ $reason }
error-dmde-geometry = DMDE 日志描述的是另一块磁盘（{ $log }），而源为 { $source }。仅在确定时使用 --force。
error-internal = 内部错误：{ $reason }
error-cancelled = 已停止。目前找到的内容都保存在会话中。
error-preflight = 未写入任何内容。请先解决以下问题：

hint-admin = 读取磁盘需要 Windows 管理员权限，或在 Linux 和 macOS 上使用 sudo。
