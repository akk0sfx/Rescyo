# Rescyo command-line strings. Machine output (JSON) is never localized.

app-about = Free, open-source data recovery that never writes to your disk.

disks-none = No disks found. Reading disks may need administrator or root rights.
disks-size-unknown = size unknown (needs administrator or root rights)
disks-removable = removable

scan-start = Scanning { $source }…
phase-probe = Looking for volumes
phase-deep-scan = Reading the whole source
phase-walk = Rebuilding folders
phase-lost-folders = Placing lost folders
scan-volume = Volume { $id }: { $fs }, cluster { $cluster }, { $files ->
        [one] { $files } file
       *[other] { $files } files
    }, { $dirs ->
        [one] { $dirs } folder
       *[other] { $dirs } folders
    }, { $deleted } deleted, { $lost } lost
scan-volume-skipped = Volume { $id }: { $fs } at { $offset } (score { $score }), not walked
scan-no-volumes = No volumes were found. Try a deep scan with --deep.
scan-bad-sectors = { $count ->
        [one] { $count } unreadable sector was skipped and recorded.
       *[other] { $count } unreadable sectors were skipped and recorded.
    }
scan-done = Session saved: { $session }

import-summary = DMDE volume { $index }: cluster { $cluster }, data at { $offset }, { $folders } folders in the log. { $seeds } directory starts used, { $offgrid } dropped as off the cluster grid.

ls-total = { $count ->
        [one] { $count } entry
       *[other] { $count } entries
    }

recover-plan = { $files ->
        [one] { $files } file
       *[other] { $files } files
    }, { $size }, to { $target }
recover-done = Recovered { $files ->
        [one] { $files } file
       *[other] { $files } files
    } ({ $size }). Report: { $report }
recover-intact = { $count } files checked against their format and intact.
recover-damaged = { $count ->
        [one] { $count } file does not read correctly as its format; see the report.
       *[other] { $count } files do not read correctly as their format; see the report.
    }
recover-partial = { $count ->
        [one] { $count } file has unreadable parts filled with zeros; see the report.
       *[other] { $count } files have unreadable parts filled with zeros; see the report.
    }
recover-guessed = { $count ->
        [one] { $count } file was read as contiguous without allocation data. Check it before trusting it.
       *[other] { $count } files were read as contiguous without allocation data. Check them before trusting them.
    }
recover-resumed = { $count } files were already in the target from an earlier run and were verified.
recover-failed = { $count } files could not be written; see the report.

info-source = Source: { $path }
info-source-unverified = The session was attached to a source that does not match its fingerprint. Results are not verified.
info-volumes = Volumes: { $count }
info-entries = Entries: { $count }

problem-target-on-source-device = The target is on the same physical disk as the source ({ $devices }). Choose a folder on another disk.
problem-unverified-target = Rescyo cannot tell which disk the target is on ({ $reason }). Choose another folder, or use --allow-unverified-target if you are sure it is a different disk.
problem-source-devices-unknown = Rescyo cannot tell which disk the source is on ({ $reason }).
problem-insufficient-space = Not enough free space: { $needed } needed, { $free } free.
problem-file-too-large = { $path } is { $size }, larger than the target file system allows ({ $limit }). Use a target formatted as exFAT or NTFS.
problem-nothing-selected = Nothing matches the selection.

error-source-open = Cannot open { $source }: { $reason }
error-image-unsupported = Cannot open { $source }: { $reason }. Convert it to a raw image (for example with qemu-img) and open that.
error-parent-missing = { $source } is a differencing image and its parent was not found (tried: { $tried }). Put the parent image next to it.
error-source-mismatch = { $source } is not the source this session was made from ({ $reason }). Use --force-rebind only if you are sure.
error-session-exists = { $path } already exists. Rescyo never overwrites a session; choose another file name.
error-session-invalid = { $path } is not a Rescyo session: { $reason }
error-session-on-source = The session file would be on the source disk ({ $devices }). Save it on another disk.
error-degraded = The source is degrading: { $count } unreadable sectors. Stop now and image it first (for example with GNU ddrescue), then scan the image. Use --continue-on-errors to go on anyway.
error-gone = The source disappeared: { $reason }
error-dmde-parse = Cannot read the DMDE log: { $reason }
error-dmde-geometry = The DMDE log describes another disk ({ $log }), the source is { $source }. Use --force only if you are sure.
error-internal = Internal error: { $reason }
error-cancelled = Stopped. The session keeps everything found so far.
error-preflight = Nothing was written. Fix these problems first:

hint-admin = Reading disks needs administrator rights on Windows or sudo on Linux and macOS.

## Imaging
image-start = Imaging { $source } to { $image } (map: { $map })…
container-opened = { $format } image: virtual disk of { $size } in { $files } file(s).
container-note = Image note: { $note }
phase-image-copy = Copying
phase-image-trim = Trimming bad areas
phase-image-scrape = Scraping bad areas
phase-image-retry = Retrying bad sectors
image-rescued = rescued
image-bad-short = bad
image-done = Image complete: { $rescued } of { $size } rescued.
image-bad = { $count ->
        [one] 1 bad area ({ $size }) could not be read.
       *[other] { $count } bad areas ({ $size }) could not be read.
    } They are zeros in the image and marked in the map; pass --map when scanning the image.
error-image-exists = { $path } already exists and has no map. Rescyo never overwrites an image; choose another file name or pass its --map.
error-map-invalid = Cannot read the map { $path }: { $reason }
error-map-mismatch = The map describes a disk of { $map }, but the source has { $source }.
error-image-cancelled = Stopped. The map is saved; run the same command again to continue.
hint-image-resume = The map is saved. Reconnect the disk and run the same command again to continue.

## Partition search
partitions-start = Searching { $source } for boot sectors…
partitions-table = Partition table: { $table }
partitions-none = No volumes were found.
partition-listed = #{ $number } at { $offset }: { $fs }{ $label }, { $size }, in the table ({ $boot })
partition-lost = Lost partition at { $offset } (byte { $bytes }): { $fs }{ $label }, { $size }, not in the table ({ $boot })
partition-conflicting = Volume at { $offset } (byte { $bytes }): { $fs }{ $label }, { $size }, overlaps a table entry ({ $boot })
partition-nested = Volume inside another at { $offset }: { $fs }{ $label }, { $size } ({ $boot })
partition-empty = #{ $number } at { $offset }, { $size }: no volume found at its start
partition-boot-both = boot sector and backup
partition-boot-primary = boot sector
partition-boot-backup = backup boot sector only
partition-boot-none = no boot sector
partition-unpartitioned = No partition table: the whole disk is one volume, { $fs }{ $label }, { $size } ({ $boot })
scan-carved = Volume { $id }: { $files ->
        [one] 1 file
       *[other] { $files } files
    } carved by their signatures (in folders by type; their content is checked on recovery)

## Terminal interface
tui-step-source = Source
tui-step-scan = Scan
tui-step-browse = Browse
tui-step-recover = Recover
tui-search = Search { $count } files…  (/)
tui-fact-cluster = cluster
tui-fact-data = data at
tui-fact-folders = folders
tui-fact-files = files
tui-col-name = Name
tui-col-size = Size
tui-col-modified = Modified
tui-col-assembly = Assembly
tui-col-integrity = Integrity
tui-intact = Intact
tui-partial = Partial
tui-damaged = Damaged
tui-unverified = Unverified
tui-empty = No files here.
tui-insp-runs = Fragments
tui-insp-state = State
tui-insp-kind = Kind
tui-deleted = deleted
tui-live = present
tui-stream = named stream
tui-note-lost = { $count } folders whose parent is lost are under _LOST_FOLDERS.
tui-scanning = Scanning { $source }
tui-recover-to = Recover to
tui-no-target = press T to choose a folder on another disk
tui-preflight-ok = Preflight: different device · { $free } free · names OK
tui-preflight-problems = { $count } problems: { $first }
tui-recover-button = R  Recover { $count } files
tui-input-target = Recover to folder
tui-input-hint = A folder on a different disk. Enter to confirm, Esc to cancel.
tui-running = Recovering… { $done } of { $total }  (Esc stops)
tui-cancelled = Stopped. Files already written are complete and listed in the report.
tui-source-changed = The source no longer matches this session. Use the command line with --force-rebind if you are sure.
tui-help-title = Keys
tui-help =
    ↑ ↓ or j k      move          Tab        switch between folders and files
    → ← or Enter    open, close    Space      mark a file    a   mark all here
    /               search         d          show or hide deleted files
    s               named streams  t          choose the target folder
    r               recover        q or Esc   quit
tui-keys =  Tab panes · Space mark · / search · d deleted · t target · r recover · ? help · q quit
