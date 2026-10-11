//! Deep scan for NTFS: boot sectors and MFT records anywhere on the Source.

use std::ops::Range;

use rescyo_fs::{CancelToken, Evidence, Phase, ScanSink};
use rescyo_io::bytes::{slice_at, u16_at, u32_at};
use rescyo_io::{GuardError, GuardedReader};

use crate::boot::{Found, NtfsGeometry, parse_boot};

const CHUNK: u64 = 1 << 20;
const SECTOR: usize = 512;
const MAX_HITS: usize = 8_000_000;

/// A sector that starts an MFT record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHit {
    pub offset: u64,
    /// Record number stored in the record (NTFS 3.1+).
    pub number: Option<u32>,
    /// Bytes allocated for the record (its size).
    pub alloc: u32,
}

#[derive(Debug, Clone, Default)]
pub struct MftScan {
    pub range: Range<u64>,
    pub boots: Vec<(u64, NtfsGeometry)>,
    pub records: Vec<RecordHit>,
}

/// Incremental scanner: feed it the Source in order, then [`NtfsScanner::finish`].
#[derive(Debug)]
pub struct NtfsScanner {
    out: MftScan,
}

impl NtfsScanner {
    pub fn new(range: Range<u64>) -> Self {
        Self {
            out: MftScan {
                range,
                ..MftScan::default()
            },
        }
    }

    /// Inspects `chunk`, which starts at absolute sector-aligned offset `at`.
    pub fn feed(&mut self, at: u64, chunk: &[u8]) {
        let out = &mut self.out;
        for (i, s) in chunk.as_chunks::<SECTOR>().0.iter().enumerate() {
            let off = at + (i * SECTOR) as u64;
            if s.starts_with(b"FILE") && out.records.len() < MAX_HITS {
                let alloc = u32_at(s, 28).unwrap_or(0);
                if alloc.is_power_of_two() && (512..=65536).contains(&alloc) {
                    let number = (u16_at(s, 4).unwrap_or(0) >= 0x30)
                        .then(|| u32_at(s, 44))
                        .flatten();
                    out.records.push(RecordHit {
                        offset: off,
                        number,
                        alloc,
                    });
                }
            } else if slice_at(s, 3, 8) == Some(b"NTFS    ")
                && out.boots.len() < 4096
                && let Some(g) = parse_boot(s, off)
            {
                out.boots.push((off, g));
            }
        }
    }

    pub fn finish(self) -> MftScan {
        self.out
    }
}

pub fn deep_scan(
    r: &GuardedReader,
    range: Range<u64>,
    sink: &mut dyn ScanSink,
    cancel: &CancelToken,
) -> Result<MftScan, GuardError> {
    let end = range.end.min(r.geometry().size);
    let start = range.start - range.start % SECTOR as u64;
    let mut scanner = NtfsScanner::new(start..end);
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

/// Volume candidates from a scan: each boot sector read as a primary and as a backup (the
/// backup sits in the volume's last sector), confirmed by MFT records where the boot sector
/// says the MFT or its mirror is.
pub fn infer_volumes(r: &GuardedReader, scan: &MftScan) -> Vec<Found> {
    let records: std::collections::HashSet<u64> = scan.records.iter().map(|h| h.offset).collect();
    let mut out: Vec<Found> = Vec::new();
    for (off, g) in &scan.boots {
        let mut hyps = vec![(g.clone(), Evidence::BootSector { offset: *off }, 1000u64)];
        if let Some(vol) = off.checked_sub(g.size) {
            let mut b = g.clone();
            let d = g.volume_offset - vol;
            b.volume_offset = vol;
            b.mft_offset -= d;
            b.mftmirr_offset = b.mftmirr_offset.map(|m| m - d);
            hyps.push((b, Evidence::BackupBootSector { offset: *off }, 900));
        }
        for (h, ev, base) in hyps {
            let confirmed =
                records.contains(&h.mft_offset) || h.mftmirr_offset.is_some_and(|m| records.contains(&m));
            if !confirmed {
                continue;
            }
            if out.iter().any(|f| f.geometry == h) {
                continue;
            }
            out.push(Found {
                geometry: h,
                evidence: vec![ev, Evidence::MftRecords { count: 1 }],
                score: base + 100,
            });
        }
    }
    if out.is_empty() {
        out.extend(from_records(r, scan));
    }
    out.sort_by_key(|f| std::cmp::Reverse(f.score));
    out
}

/// Geometry without any boot sector. Record hits group into runs whose start is
/// `offset - number * size`: the largest is the MFT, a separate run of records 0..=3 is
/// `$MFTMirr`. Record 0 says at which cluster the MFT starts and record 1 where the mirror
/// starts, which gives two equations `volume + lcn * cluster = offset` to solve.
fn from_records(r: &GuardedReader, scan: &MftScan) -> Option<Found> {
    use std::collections::HashMap;
    let mut groups: HashMap<(u64, u32), Vec<u32>> = HashMap::new();
    for h in &scan.records {
        let Some(n) = h.number else { continue };
        let Some(base) = h.offset.checked_sub(u64::from(n) * u64::from(h.alloc)) else {
            continue;
        };
        groups.entry((base, h.alloc)).or_default().push(n);
    }
    let (&(mft, rs), _) = groups.iter().max_by_key(|(_, v)| v.len())?;
    let mirror = groups
        .iter()
        .filter(|&(&(b, a), v)| {
            b != mft && a == rs && v.iter().all(|&n| n <= 3) && v.contains(&0) && v.contains(&1)
        })
        .map(|(&(b, _), _)| b)
        .min()?;
    let first_lcn = |at: u64, want: u32| -> Option<u64> {
        let mut rec = vec![0u8; usize::try_from(rs).ok()?];
        if !r.read(at, &mut rec).ok()?.is_clean() || !crate::record::apply_fixup(&mut rec, 512) {
            return None;
        }
        let h = crate::record::header(&rec)?;
        if h.number != Some(want) {
            return None;
        }
        crate::record::attributes(&rec, &h)
            .into_iter()
            .find_map(|a| match a.body {
                crate::record::AttrBody::NonResident { runs, .. }
                    if a.kind == crate::record::ATTR_DATA && a.name.is_empty() =>
                {
                    crate::record::decode_runs(runs, 4096, u64::MAX)?.first()?.lcn
                }
                _ => None,
            })
    };
    // Record 0 ($MFT) and record 1 ($MFTMirr) from whichever copy survived.
    let l0 = first_lcn(mft, 0).or_else(|| first_lcn(mirror, 0))?;
    let l1 = first_lcn(mft + u64::from(rs), 1).or_else(|| first_lcn(mirror + u64::from(rs), 1))?;
    let (dl, doff) = if l1 > l0 {
        (l1 - l0, mirror.checked_sub(mft)?)
    } else {
        (l0 - l1, mft.checked_sub(mirror)?)
    };
    if dl == 0 || !doff.is_multiple_of(dl) {
        return None;
    }
    let cluster = doff / dl;
    let volume = mft.checked_sub(l0 * cluster)?;
    let g = NtfsGeometry {
        volume_offset: volume,
        bytes_per_sector: 512,
        cluster_size: u32::try_from(cluster).ok()?,
        size: scan.range.end.saturating_sub(volume),
        mft_offset: mft,
        mftmirr_offset: Some(mirror),
        record_size: rs,
    };
    g.is_sane().then(|| Found {
        geometry: g,
        evidence: vec![Evidence::MftRecords {
            count: groups.values().map(|v| v.len() as u64).sum(),
        }],
        score: 600,
    })
}
