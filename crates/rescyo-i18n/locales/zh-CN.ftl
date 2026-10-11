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
recover-intact = { $count } 个文件已按其格式校验，完好无损。
recover-damaged = { $count } 个文件无法按其格式正确读取，请查看报告。
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
error-image-unsupported = 无法打开 { $source }：{ $reason }。请先将其转换为原始镜像（例如使用 qemu-img）再打开。
error-parent-missing = { $source } 是差异镜像，未找到其父镜像（已尝试：{ $tried }）。请将父镜像放在它旁边。
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

## Imaging
image-start = 正在将 { $source } 制作镜像到 { $image }（映射文件：{ $map }）…
container-opened = { $format } 镜像：虚拟磁盘 { $size }，共 { $files } 个文件。
container-note = 镜像提示：{ $note }
phase-image-copy = 复制
phase-image-trim = 修剪坏区
phase-image-scrape = 逐扇区读取坏区
phase-image-retry = 重试坏扇区
image-rescued = 已救回
image-bad-short = 坏
image-done = 镜像完成：已救回 { $rescued },共 { $size }。
image-bad = { $count } 个坏区（{ $size }）无法读取。它们在镜像中为零,并已在映射文件中标记;扫描镜像时请使用 --map。
error-image-exists = { $path } 已存在且没有映射文件。Rescyo 从不覆盖镜像;请换一个文件名,或指定它的 --map。
error-map-invalid = 无法读取映射文件 { $path }：{ $reason }
error-map-mismatch = 映射文件描述的磁盘大小为 { $map },而源为 { $source }。
error-image-cancelled = 已停止。映射文件已保存;再次运行同一命令即可继续。
hint-image-resume = 映射文件已保存。重新连接磁盘后再次运行同一命令即可继续。

## Partition search
partitions-start = 正在 { $source } 上搜索引导扇区…
partitions-table = 分区表：{ $table }
partitions-none = 未找到任何卷。
partition-listed = #{ $number } 位于 { $offset }：{ $fs }{ $label }，{ $size }，在分区表中（{ $boot }）
partition-lost = 丢失的分区位于 { $offset }（字节 { $bytes }）：{ $fs }{ $label }，{ $size }，不在分区表中（{ $boot }）
partition-conflicting = 位于 { $offset }（字节 { $bytes }）的卷：{ $fs }{ $label }，{ $size }，与分区表中的条目重叠（{ $boot }）
partition-nested = 位于 { $offset } 的嵌套卷：{ $fs }{ $label }，{ $size }（{ $boot }）
partition-empty = #{ $number } 位于 { $offset }，{ $size }：起始处未找到卷
partition-boot-both = 引导扇区及其备份
partition-boot-primary = 引导扇区
partition-boot-backup = 仅有备份引导扇区
partition-boot-none = 无引导扇区
partition-unpartitioned = 没有分区表：整个磁盘是一个卷，{ $fs }{ $label }，{ $size }（{ $boot }）
