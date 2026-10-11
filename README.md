# Rescyo

**English** · [Русский](README.ru.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [한국어](README.ko.md)

Rescyo is a free, open-source data recovery tool for damaged, formatted or partially unreadable disks, memory cards and USB flash drives. It runs as a single portable binary on Windows 7–11, Linux and macOS.

> **Status: pre-alpha.** Nothing is released yet. Do not rely on it for important data until v0.1.0 passes its byte-for-byte recovery gate.

Free open-source alternative to DMDE, R-Studio and UFS Explorer. Not affiliated with DMDE Software, R-Tools Technology or SysDev Laboratories; DMDE, R-Studio and UFS Explorer are trademarks of their respective owners.

## Principles

- **Never write to the source.** The disk being recovered is opened read-only, enforced by types: the I/O layer has no write operation. Recovered files can never be saved to the same physical device, checked by device, not by drive letter.
- **Be honest about every file.** Each recovered file says how it was assembled (FAT chain, MFT runlist, or a contiguous guess) and whether its content checks out.
- **Spare dying media.** Unreadable sectors are read once, recorded and never hammered again; when a disk degrades, Rescyo stops and tells you to image it first.
- **Hostile input is the default.** Disk structures are parsed without `unsafe`, with hard limits on recursion, loops and allocations, and every parser is fuzzed.
- **Automation first.** Everything is available from the CLI with stable, versioned JSON output.

## Planned for v0.1

- FAT12/16/32, exFAT and NTFS: lost volume search, folder tree reconstruction, deleted files
- Recovery of entire folder trees with no file-count limits
- Import of DMDE scan logs (`devscan.log`) to skip a rescan
- JSON and CSV reports with SHA-256 for every file
- Interface in English, Russian, Simplified Chinese, Japanese and Korean

See the [roadmap](docs/roadmap.md) for later versions: GUI and TUI, ext4/HFS+/APFS/Btrfs, disk imaging with ddrescue-compatible maps, RAID, file carving with integrity checks, and an MCP server for AI agents.

## Building

```sh
cargo build --release
```

The binary is `target/release/rescyo`. Windows 7 builds use a pinned nightly toolchain; see [CONTRIBUTING.md](CONTRIBUTING.md).

## Contributing

Contributions are welcome under the Developer Certificate of Origin. Read [CONTRIBUTING.md](CONTRIBUTING.md) first; the project glossary is in [CONTEXT.md](CONTEXT.md) and design decisions in [docs/adr](docs/adr).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). This program comes with ABSOLUTELY NO WARRANTY.
