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
