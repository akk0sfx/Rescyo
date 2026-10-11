//! Deep scan: one sequential pass over a range of the Source collecting every structure that
//! can locate a FAT or exFAT volume without its boot sector.

use std::ops::Range;

use rescyo_fs::{CancelToken, FsKind, Phase, ScanSink};
use rescyo_io::bytes::{u8_at, u16_at};
use rescyo_io::{GuardError, GuardedReader};

use crate::boot::{parse_exfat_boot, parse_fat_boot};
use crate::exdir::{self, ExContext};
use crate::geometry::FatGeometry;
use crate::table::is_fat_head;

const CHUNK: u64 = 1 << 20;
const SECTOR: usize = 512;
/// Caps per kind of hit, so a hostile disk full of fake structures cannot exhaust memory.
const MAX_HITS: usize = 4_000_000;
/// Permissive validation context while geometry is unknown.
const FRAG_CTX: crate::dir::DirContext = crate::dir::DirContext {
    fat32: true,
    max_cluster: 0x0FFF_FFF6,
    max_size: u64::MAX,
    codepage: rescyo_fs::codepage::Codepage::Cp437,
};

/// A sector that begins with `.` and `..` directory entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirStart {
    pub offset: u64,
    /// Self cluster read with the FAT32 high word.
    pub me: u32,
    pub parent: u32,
    /// Self and parent clusters read without the high word (FAT12/16).
    pub me16: u16,
    pub parent16: u16,
}

/// Which allocation-table heads matched at an offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatHead {
    pub offset: u64,
    /// First byte: the media descriptor at a real table start (0xF8, 0xF0, …), 0xFF inside a
    /// table full of end marks.
    pub media: u8,
    pub fat12: bool,
    pub fat16: bool,
    pub fat32: bool,
    pub exfat: bool,
}

/// Everything a deep scan found.
#[derive(Debug, Clone, Default)]
pub struct DeepScan {
    pub range: Range<u64>,
    /// Boot sectors anywhere, as geometries for a volume starting at that sector.
    pub boots: Vec<(u64, FatGeometry)>,
    pub fat_heads: Vec<FatHead>,
    pub dir_starts: Vec<DirStart>,
    /// Sectors full of valid FAT directory entries but without `.`: directory fragments
    /// (continuation clusters), the same idea as DMDE's `dirfrags`.
    pub dir_frags: Vec<u64>,
    /// Sectors that begin with a valid exFAT entry set.
    pub ex_dirs: Vec<u64>,
    /// Sectors that begin like an exFAT root directory (bitmap or up-case entry up front).
    pub ex_roots: Vec<u64>,
}

/// Incremental scanner: feed it the Source in order, chunk by chunk, then [`FatScanner::finish`].
/// Lets one pass over a disk serve several file-system scanners.
#[derive(Debug)]
pub struct FatScanner {
    out: DeepScan,
    ex: ExContext,
}

impl FatScanner {
    pub fn new(range: Range<u64>) -> Self {
        // Permissive context: geometry is unknown while scanning.
        let ex = ExContext {
            max_cluster: u32::MAX - 1,
            cluster_size: 1,
            max_size: u64::MAX,
        };
        Self {
            out: DeepScan {
                range,
                ..DeepScan::default()
            },
            ex,
        }
    }

    /// Inspects `chunk`, which starts at absolute sector-aligned offset `at`.
    pub fn feed(&mut self, at: u64, chunk: &[u8]) {
        for (i, s) in chunk.as_chunks::<SECTOR>().0.iter().enumerate() {
            inspect(s, at + (i * SECTOR) as u64, &self.ex, &mut self.out);
        }
    }

    pub fn finish(self) -> DeepScan {
        self.out
    }
}

/// Scans `range` sector by sector. Progress goes to `sink` as [`Phase::DeepScan`].
pub fn deep_scan(
    r: &GuardedReader,
    range: Range<u64>,
    sink: &mut dyn ScanSink,
    cancel: &CancelToken,
) -> Result<DeepScan, GuardError> {
    let end = range.end.min(r.geometry().size);
    let start = range.start - range.start % SECTOR as u64;
    let mut scanner = FatScanner::new(start..end);
    let mut buf = vec![0u8; usize::try_from(CHUNK).unwrap_or(1 << 20)];
    let mut at = start;
    while at < end && !cancel.is_cancelled() {
        let n = usize::try_from((end - at).min(CHUNK)).unwrap_or(0);
        let chunk = buf.get_mut(..n).unwrap_or_default();
        r.read(at, chunk)?;
        scanner.feed(at, chunk);
        at += n as u64;
        sink.progress(Phase::DeepScan, at - start, Some(end - start));
    }
    Ok(scanner.finish())
}

fn inspect(s: &[u8], off: u64, ex: &ExContext, out: &mut DeepScan) {
    let first = u8_at(s, 0).unwrap_or(0);
    if u16_at(s, 510) == Some(0xAA55) && out.boots.len() < MAX_HITS {
        if let Some(g) = parse_fat_boot(s, off) {
            out.boots.push((off, g));
        } else if let Some(g) = parse_exfat_boot(s, off) {
            out.boots.push((off, g));
        }
    }
    if matches!(first, 0xF0 | 0xF8..=0xFF) && out.fat_heads.len() < MAX_HITS {
        let h = FatHead {
            offset: off,
            media: first,
            fat12: is_fat_head(s, FsKind::Fat12),
            fat16: is_fat_head(s, FsKind::Fat16),
            fat32: is_fat_head(s, FsKind::Fat32),
            exfat: is_fat_head(s, FsKind::Exfat),
        };
        if h.fat12 || h.fat16 || h.fat32 || h.exfat {
            out.fat_heads.push(h);
        }
    }
    if first != b'.'
        && first != 0
        && first != 0xE5
        && out.dir_frags.len() < MAX_HITS
        && crate::dir::looks_like_dir(s, &FRAG_CTX, true)
    {
        let (valid, invalid) = crate::dir::score(s, &FRAG_CTX);
        // Most of the sector must be real entries, not a lucky slot or two.
        if invalid == 0 && valid >= 4 {
            out.dir_frags.push(off);
        }
    }
    if first == b'.' && out.dir_starts.len() < MAX_HITS {
        if let Some((me, parent)) = crate::dir::dir_start(s, true) {
            let lo = |c: u32| u16::try_from(c & 0xFFFF).unwrap_or(0);
            out.dir_starts.push(DirStart {
                offset: off,
                me,
                parent,
                me16: lo(me),
                parent16: lo(parent),
            });
        } else if let Some((me, parent)) = crate::dir::dir_start(s, false) {
            let lo = |c: u32| u16::try_from(c).unwrap_or(0);
            out.dir_starts.push(DirStart {
                offset: off,
                me,
                parent,
                me16: lo(me),
                parent16: lo(parent),
            });
        }
    }
    // Directory clusters may start with the tail of a set begun in the previous cluster, so any
    // sector that holds a complete, checksummed set counts.
    if matches!(first, 0x81..=0x85 | 0xC0 | 0xC1 | 0x05 | 0x40 | 0x41) && out.ex_dirs.len() < MAX_HITS {
        let (entries, info) = exdir::parse_exdir(s, ex, 16);
        let root = info.bitmap.is_some_and(|(c, _)| c >= 2) && info.upcase.is_some_and(|(c, _, _)| c >= 2);
        if root || !entries.is_empty() {
            out.ex_dirs.push(off);
        }
        if root {
            out.ex_roots.push(off);
        }
    }
}
