# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow [Semantic Versioning](https://semver.org/). The `--json` output has its own schema version (currently 1).

## [Unreleased]

### Added

- **Imaging**: `rescyo image` copies a failing disk to an image file in ddrescue-style passes (copy with skipping, trim, scrape, optional retries), keeps a GNU ddrescue-compatible map, saves it atomically and resumes an interrupted copy; existing images are never overwritten.
- `scan`, `import-dmde` and `recover` take `--map` so the unread areas of an image count as unreadable sectors; `recover` no longer re-reads sectors its session already found unreadable.

## [0.1.0] — 2026-10-11

First alpha: a command-line tool that recovers whole folder trees from FAT12/16/32, exFAT and NTFS, including damaged volumes, without ever writing to the source.

### Added

- **Read-only sources**: disk images and physical disks on Windows, Linux and macOS through a type with no write operation; unreadable sectors are read once, zero-filled, recorded in a bad-sector map and never re-read; a degradation alarm stops work and advises imaging first.
- **Partition tables**: MBR with EBR chains, GPT with backup fallback (also without a protective MBR).
- **FAT12/16/32 and exFAT**: boot sectors and backups; chains through the primary then the mirror FAT; long names validated by checksum, deleted long names recovered with the erased first character restored; exFAT entry sets by checksum; deleted files and folders; OEM code pages 437/850/866/932/936/949/950 with detection.
- **NTFS**: MFT records with fixups; the tree from parent references (survives a destroyed root or indexes); resident and non-resident data, sparse runs, extension records; deleted records.
- **Lost volumes and folders**: a deep scan reads the source once and infers geometry without any boot sector (directory-start votes, allocation tables by signature, exFAT up-case checksum, NTFS `$MFT`/`$MFTMirr` equations); folders whose parents are gone are placed under `_LOST_FOLDERS`.
- **DMDE scan import**: `import-dmde` reuses a `devscan.log`'s geometry and folder list as verified hints, without a full rescan.
- **Sessions** in one SQLite file, bound to the source by a fingerprint; rebinding to an image of the same disk.
- **Recovery** to another physical disk only (checked by device, not by drive letter; fails closed), with a full preflight (space, FAT32 4 GiB limit, names), names made safe for Windows, macOS and Linux, no overwriting, resumable runs, original timestamps, and JSON/CSV reports with SHA-256, the assembly method of every file and unreadable ranges.
- **CLI** `rescyo` with `disks`, `scan`, `import-dmde`, `volumes`, `ls`, `recover`, `info`; stable `--json` envelopes and `--json-stream` progress; documented exit codes; interface in English, Russian, Simplified Chinese, Japanese and Korean.
- **Quality**: a containerised test corpus (7 scenarios, 42 damage combinations) recovered byte for byte through the release binary (Gate 1); six fuzz targets; builds for Windows 7–11 (x86, x64, arm64), Linux musl (x86_64, i686, aarch64, armv7, riscv64, loongarch64) and macOS (universal).

### Known limitations

- No signed binaries yet: build from source or take CI artifacts until code signing is in place.
- NTFS compressed and encrypted files, alternate data streams, ext4/HFS+/APFS/Btrfs, imaging, carving and the GUI arrive in later versions (see the roadmap).
- Translations other than English and Russian await native review.

[0.1.0]: https://github.com/akk0sfx/Rescyo/releases/tag/v0.1.0
