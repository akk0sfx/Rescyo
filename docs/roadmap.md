# Roadmap

Stages advance when their gate passes, not by date.

| Version | Scope | Gate |
|---|---|---|
| v0.1 | CLI with JSON output; FAT12/16/32, exFAT, NTFS (MFT, deleted files); folder-tree recovery; DMDE scan import; JSON/CSV reports with SHA-256; five UI languages; Windows 7–11, Linux, macOS | Gate 1: test images recovered byte-for-byte |
| v0.2–0.4 | Disk imaging with ddrescue-compatible maps; lost partition search; NTFS compression, sparse files, ADS, WOF; E01/VHD/VHDX/VMDK/qcow2/DMG; ext4, HFS+, APFS, Btrfs (experimental); file carving with integrity checks; hex editor; cluster map; TUI and GUI | Gate 2: fuzzing clean, Windows 7 CI green |
| v1.0 | MCP server for AI agents; virtual RAID 0/1/4/5/6 and JBOD; Rhai scripts for I/O error handling; bootable Linux live-USB; packaging | Gate 3: signed builds on all platforms |
| v2 | Remote agent; ReFS; BitLocker/LUKS; WASM plugins; NTFS repair; RAID auto-detection | — |

File-system support comes in two tiers ([ADR-0005](adr/0005-two-tier-file-system-depth.md)): FAT, exFAT and NTFS get full recovery depth; ext4, HFS+, APFS and Btrfs ship as experimental in v1.0.
