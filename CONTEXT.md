# Rescyo

Rescyo recovers files from damaged, formatted or partially unreadable storage without ever writing to it. This glossary fixes the words used in code, CLI, reports and docs.

## Language

### Sources and sessions

**Source**:
The read-only thing being analysed: a physical disk, an image file, or (later) a stream from a remote agent.
_Avoid_: device (too narrow), drive, input

**Session**:
A persisted analysis result for exactly one Source: found volumes, found entries and an action journal, stored in one file.
_Avoid_: scan (a Scan is an activity that fills a Session), project, case

**Scan**:
The activity of examining a Source and filling a Session. A quick scan probes typical offsets and partition starts; a deep scan reads the whole Source and can be paused and resumed.
_Avoid_: analysis, search

**Source fingerprint**:
The identity of a Source: size, sector size, hardware identity when available, and hashes of fixed sample regions.
_Avoid_: disk ID, serial

**Rebind**:
Attaching an existing Session to a different Source whose Source fingerprint matches (typically an image of the original disk); recorded in the journal.
_Avoid_: reattach, reopen

**Forced rebind**:
A Rebind accepted despite a fingerprint mismatch; every Report from that Session marks the Source as unverified.

**Bad-sector map**:
The per-Session record of which Source ranges are unread, readable or unreadable; known-bad ranges are never re-read unless explicitly requested. Exportable as a ddrescue mapfile.
_Avoid_: error log, bad block list

**Degradation alarm**:
The automatic pause of a Scan or Recovery when read errors or slowness cross a threshold, advising to image the Source first.

**Imaging**:
Copying a whole Source to an image file in passes that take the easy data first and the damaged areas last, so a failing disk is read as little as possible. An interrupted Imaging continues from its Image map.
_Avoid_: cloning, backup

**Image map**:
The ddrescue mapfile kept next to an image, saying for every byte range whether it was rescued, is still pending or is unreadable. A Scan of the image takes it as the image's Bad-sector map.
_Avoid_: log file

### Volumes and hints

**Volume candidate**:
A hypothesis that a file system of a given type and geometry (cluster size, offset of cluster #2) exists on a Source.
_Avoid_: partition (a partition is a table entry; a Volume candidate may have none)

**Lost partition**:
A volume found by its boot sectors (and confirmed by its tables) that the partition table does not list and that overlaps nothing the table lists. Rescyo reports it; rewriting the table is a separate, expert decision.
_Avoid_: deleted partition, missing drive

**Volume**:
The Volume candidate chosen for analysis in a Session.

**Hint**:
An untrusted external suggestion about a Source's structure (a Volume candidate or a directory start), e.g. from a DMDE scan log. A Hint asserts nothing until confirmed by reading the Source.
_Avoid_: import data, DMDE result

**Directory seed**:
A confirmed directory start used to begin tree reconstruction without a full scan.

### What is found

**Entry**:
A file or directory discovered in a Session, live or deleted, with its name, path, size, timestamps and byte extents.
_Avoid_: file record, item, node, object

**Named stream**:
A second, named content of a file (an NTFS alternate data stream), listed and recovered as `file:stream` beside its file, and only when asked for.
_Avoid_: fork, ADS (in user-facing text)

**Assembly method**:
How an Entry's byte extents were determined: `fat_chain`, `fat_chain_mirror`, `exfat_bitmap_contiguous`, `mft_runlist`, `ext4_extents`, `block_map`, `contiguous_assumed`, `carved`. Ordered from most to least trustworthy.
_Avoid_: reliability, confidence, source

**Carving**:
Finding files by their own structure (signature and format) where no file system describes them. A carved file is assumed contiguous, so its content is always verified; carved files are kept in a separate `carved` pseudo-volume, never mixed with a file system's Entries.
_Avoid_: raw recovery, signature search (in user-facing text)

**Contiguous assumption**:
The Assembly method `contiguous_assumed`: no allocation metadata survived, so the Entry is read as one run from its first cluster. Always a guess.
_Avoid_: linear recovery, raw read

**Integrity**:
What content verification concluded about recovered bytes: `intact`, `partial` (some sectors unreadable, zero-filled), `damaged` (format check failed), `unverified`. Independent of Assembly method.
_Avoid_: status, health, quality

### Getting files out

**Recovery**:
Copying selected Entries from a Source to a Target. The only operation that writes, and never to the Source.
_Avoid_: export, restore, save, extraction

**Target**:
The destination directory of a Recovery; must resolve to physical devices disjoint from the Source.
_Avoid_: output, destination drive

**Unverified target**:
A Target whose physical devices cannot be determined; Recovery refuses it unless explicitly allowed, and the Report records the override.

**Preflight**:
The complete set of checks run before the first byte of a Recovery is written (space, size limits, names, paths, collisions); all problems are reported at once.
_Avoid_: validation, dry run

**Report**:
The per-Recovery record of every Entry: path, Assembly method, Integrity, SHA-256, unreadable sectors.
_Avoid_: log (the journal is the Session's log)

**Lost folder**:
A directory Entry whose parent cannot be determined; exported under `_LOST_FOLDERS/`.
_Avoid_: orphan, detached directory
