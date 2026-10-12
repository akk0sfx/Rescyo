# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow [Semantic Versioning](https://semver.org/). The `--json` output has its own schema version (currently 1).

## [Unreleased]

### Added

- **Terminal interface**: `rescyo tui SESSION` browses a session (folder tree, files with their assembly and integrity, inspector) and recovers marked files or whole folders through the same preflight; `rescyo tui SOURCE --session NEW [--deep] [--carve]` scans with a live sector strip first. Same flow and design tokens as the coming GUI, 24-bit or 256 colours, `NO_COLOR` respected, five languages; works over SSH and on live-USB consoles.
- **Virtual-disk containers as sources**: VHD (fixed, dynamic, differencing), VHDX (dynamic, fixed, differencing; an unreplayed log is replayed in memory only), VMDK (sparse, stream-optimized, flat and split descriptors, delta disks), qcow2 v2/v3 (compressed clusters, backing files) and EnCase/SMART E01/S01 evidence sets (all segments, Adler-32 and table mirrors) open directly, detected by signature or chosen with `--format`. Damage inside a container (a table entry outside its file, a chunk that fails its checksum or does not decompress) makes only that range unreadable; header copies, redundant grain directories and table mirrors are used when the primary is damaged. `rescyo image` converts any of them to a raw image. Checked against files written by qemu-img and ewfacquire (`cargo xtask containers`).
- **File carving**: `scan --deep --carve` finds JPEG, PNG, GIF, PDF, ZIP-based documents (docx, xlsx, pptx, odt, epub…), MP4/MOV/HEIF, WAV/AVI/WEBP and BMP files by their own structure where no file system describes them, follows each to its real end (never a guessed length), and keeps them in a separate `carved` volume, one folder per type; recovery verifies their content like any other file.
- **ext2/ext3/ext4 (experimental)**: intact volumes with extent trees, block maps, inline data and hashed directories; a backup superblock when the primary is gone; lost ext4 partitions found by their superblocks. Deleted names are read from directory slack where the kernel left them (Linux 5.13 and later wipes them, and zeroes a deleted inode's extents; carving recovers such files).

## [0.2.0] — 2026-10-12

Second alpha: image a dying disk before touching it, find partitions the table lost, and read the NTFS files Windows compresses.

### Added

- **Imaging**: `rescyo image` copies a failing disk to an image file in ddrescue-style passes (copy with skipping, trim, scrape, optional retries), keeps a GNU ddrescue-compatible map, saves it atomically and resumes an interrupted copy; existing images are never overwritten.
- **NTFS compression**: files compressed by NTFS (LZNT1) are rebuilt one compression unit at a time, including deleted ones; damage inside a unit marks only that unit unreadable.
- **WOF compression** (`compact /exe:xpress4k|xpress8k|xpress16k|lzx`, used by Windows 10/11 for system files): XPRESS Huffman and LZX decoders written from the public specifications, checked against wimlib.
- **Named streams** (NTFS alternate data streams): `ls --streams` and `recover --streams` show and recover them as `file:stream` (`file_stream` on Windows and macOS targets).
- Encrypted (EFS) files are reported as unreadable without the user's key instead of being copied as ciphertext.
- **Lost partition search**: `rescyo partitions` reads a disk or image once for FAT, exFAT and NTFS boot sectors (primary and backup), confirms each against the volume's FAT, root directory or MFT, and lists what it finds as listed, lost, conflicting, nested or unpartitioned, plus table entries with no volume. Nothing is written; deep scans use the same search and report lost partitions.
- `scan`, `import-dmde` and `recover` take `--map` so the unread areas of an image count as unreadable sectors; `recover` no longer re-reads sectors its session already found unreadable.

### Changed

- Sessions use schema 2 (compressed extents, named streams); Rescyo 0.1 refuses to open them rather than misreading compressed data. Schema 1 sessions still open.

### Known limitations

- Still no signed binaries: build from source or take CI artifacts until code signing is in place.
- EFS-encrypted files, WOF files backed by an external WIM, ext4/HFS+/APFS/Btrfs, disk-image formats other than raw, carving and the GUI arrive in later versions (see the roadmap).
- Partition tables are found and reported, never rewritten; table repair is planned for the expert mode in v0.4.
- Translations other than English and Russian await native review.

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

[0.2.0]: https://github.com/akk0sfx/Rescyo/releases/tag/v0.2.0
[0.1.0]: https://github.com/akk0sfx/Rescyo/releases/tag/v0.1.0
