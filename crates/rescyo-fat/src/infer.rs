//! Volume geometry from a deep scan: boot sectors wherever they survive, directory starts that
//! vote for a cluster grid, and allocation tables found by signature in front of the data area.

use std::collections::{BTreeMap, HashMap, HashSet};

use rescyo_fs::codepage::Codepage;
use rescyo_fs::{DirectorySeed, Evidence, FsKind, SeedOrigin};
use rescyo_io::{GuardError, GuardedReader};

use crate::boot::Found;
use crate::dir::{self, DirContext};
use crate::exdir::{self, ExContext};
use crate::geometry::{FatCopy, FatGeometry, RootDir};
use crate::scan::DeepScan;

/// How far before the data area allocation tables are looked for.
const FAT_WINDOW: u64 = 512 << 20;
/// Most directory starts and pointers used for voting.
const MAX_VOTERS: usize = 200_000;
const MAX_EX_POINTERS: usize = 128;
const MAX_EX_DIRS: usize = 4096;

/// All volume candidates a deep scan supports, best first.
pub fn infer_volumes(r: &GuardedReader, scan: &DeepScan) -> Result<Vec<Found>, GuardError> {
    let mut found: Vec<Found> = Vec::new();
    let heads: HashSet<u64> = scan.fat_heads.iter().map(|h| h.offset).collect();

    // 1. Boot sectors, including the "this is a backup copy" reading of each one.
    for (off, g) in &scan.boots {
        let backup_delta = match g.fs {
            FsKind::Fat32 => Some(6 * u64::from(g.bytes_per_sector)),
            FsKind::Exfat => Some(12 * u64::from(g.bytes_per_sector)),
            _ => None,
        };
        let mut hyps = vec![(g.clone(), Evidence::BootSector { offset: *off }, 1000u64)];
        if let Some(d) = backup_delta
            && off.checked_sub(d).is_some()
        {
            hyps.push((shift_back(g, d), Evidence::BackupBootSector { offset: *off }, 900));
        }
        for (h, ev, base) in hyps {
            let fat_ok = h.fats.first().is_some_and(|f| heads.contains(&f.offset));
            let score = if fat_ok { base + 100 } else { base / 3 };
            let mut evidence = vec![ev];
            if fat_ok && let Some(f) = h.fats.first() {
                evidence.push(Evidence::FatSignature { offset: f.offset });
            }
            merge(
                &mut found,
                Found {
                    geometry: h,
                    evidence,
                    score,
                },
            );
        }
    }

    // 2. FAT directory starts vote for (cluster size, data offset).
    let end = scan.range.end;
    for (cluster, data, wide, votes) in fat_votes(scan) {
        if found
            .iter()
            .any(|f| f.geometry.cluster_size == cluster && f.geometry.data_offset == data)
        {
            add_votes(&mut found, cluster, data, votes);
            continue;
        }
        if let Some(mut f) = geometry_from_grid(r, scan, cluster, data, end, Some(wide))? {
            f.evidence.push(Evidence::DirectoryVotes { count: votes });
            f.score += 300 + votes.min(500);
            merge(&mut found, f);
        }
    }

    // 3. exFAT subdirectory pointers vote for the cluster grid.
    if let Some(f) = exfat_from_votes(r, scan)? {
        merge(&mut found, f);
    }

    found.sort_by_key(|f| std::cmp::Reverse(f.score));
    Ok(found)
}

fn shift_back(g: &FatGeometry, d: u64) -> FatGeometry {
    let mut h = g.clone();
    h.volume_offset = g.volume_offset.map(|v| v - d);
    h.data_offset -= d;
    for f in &mut h.fats {
        f.offset -= d;
    }
    if let RootDir::Region { offset, len } = h.root {
        h.root = RootDir::Region {
            offset: offset - d,
            len,
        };
    }
    h
}

fn merge(found: &mut Vec<Found>, f: Found) {
    if let Some(x) = found.iter_mut().find(|x| {
        x.geometry.data_offset == f.geometry.data_offset
            && x.geometry.cluster_size == f.geometry.cluster_size
            && x.geometry.fs == f.geometry.fs
    }) {
        if f.score > x.score {
            let mut ev = std::mem::take(&mut x.evidence);
            *x = f;
            ev.retain(|e| !x.evidence.contains(e));
            x.evidence.extend(ev);
        } else {
            for e in f.evidence {
                if !x.evidence.contains(&e) {
                    x.evidence.push(e);
                }
            }
        }
    } else {
        found.push(f);
    }
}

fn add_votes(found: &mut [Found], cluster: u32, data: u64, votes: u64) {
    for f in found
        .iter_mut()
        .filter(|f| f.geometry.cluster_size == cluster && f.geometry.data_offset == data)
    {
        f.evidence.push(Evidence::DirectoryVotes { count: votes });
        f.score += votes.min(500);
    }
}

/// Grids supported by at least three directory starts and a quarter of the best grid's votes.
fn fat_votes(scan: &DeepScan) -> Vec<(u32, u64, bool, u64)> {
    let mut votes: HashMap<(u32, u64), (u64, u64)> = HashMap::new();
    for ds in scan.dir_starts.iter().take(MAX_VOTERS) {
        for shift in 9..=18u32 {
            let c = 1u64 << shift;
            let cs = 1u32 << shift;
            if ds.me >= 2
                && let Some(data) = ds.offset.checked_sub(u64::from(ds.me - 2) * c)
            {
                votes.entry((cs, data)).or_default().0 += 1;
            }
            if ds.me16 >= 2
                && let Some(data) = ds.offset.checked_sub(u64::from(ds.me16 - 2) * c)
            {
                votes.entry((cs, data)).or_default().1 += 1;
            }
        }
    }
    let best = votes.values().map(|(w, n)| (*w).max(*n)).max().unwrap_or(0);
    let mut out: Vec<(u32, u64, bool, u64)> = votes
        .into_iter()
        .filter_map(|((c, d), (w, n))| {
            let v = w.max(n);
            // Below cluster 65 536 both readings agree; only a strict majority proves FAT32.
            (v >= 3 && v * 4 >= best).then_some((c, d, w > n, v))
        })
        .collect();
    out.sort_by(|a, b| b.3.cmp(&a.3).then(a.0.cmp(&b.0)));
    out.truncate(16);
    out
}

/// Builds a FAT geometry for a known cluster grid, locating the allocation tables the way the
/// FlashRecover prototype did: by their signature in front of the data area.
pub fn geometry_from_grid(
    r: &GuardedReader,
    scan: &DeepScan,
    cluster: u32,
    data: u64,
    end: u64,
    wide: Option<bool>,
) -> Result<Option<Found>, GuardError> {
    if !cluster.is_power_of_two() || cluster < 512 || data >= end {
        return Ok(None);
    }
    if let Some((off, g)) = scan
        .boots
        .iter()
        .find(|(_, g)| g.cluster_size == cluster && g.data_offset == data)
    {
        return Ok(Some(Found {
            geometry: g.clone(),
            evidence: vec![Evidence::BootSector { offset: *off }],
            score: 1000,
        }));
    }
    let lo = data.saturating_sub(FAT_WINDOW);
    let heads: Vec<_> = scan
        .fat_heads
        .iter()
        .filter(|h| h.offset >= lo && h.offset < data)
        .collect();
    let mut evidence = Vec::new();
    let mut fats = Vec::new();
    let mut root = RootDir::Unknown;
    let mut fs = None;

    // FAT32: two copies end exactly at the data area, or one copy does.
    let f32s: Vec<u64> = heads.iter().filter(|h| h.fat32).map(|h| h.offset).collect();
    if let Some((a, b)) = f32s
        .iter()
        .flat_map(|&a| f32s.iter().map(move |&b| (a, b)))
        .filter(|&(a, b)| b > a && b - a == data - b)
        .max_by_key(|&(_, b)| b)
    {
        fats = vec![
            FatCopy {
                offset: a,
                len: b - a,
            },
            FatCopy {
                offset: b,
                len: b - a,
            },
        ];
        fs = Some(FsKind::Fat32);
    }

    // FAT12/16: two copies, then a fixed root region whose end is the data area.
    if fs.is_none() {
        let small: Vec<(u64, u8)> = heads
            .iter()
            .filter(|h| h.fat12 || h.fat16)
            .map(|h| (h.offset, h.media))
            .collect();
        let mut pairs: Vec<(u64, u64, u64, u64, u64, u32)> = Vec::new();
        for &(a, ma) in &small {
            for &(b, mb) in &small {
                let Some(len) = b.checked_sub(a).filter(|l| *l > 0) else {
                    continue;
                };
                let Some(root_off) = b.checked_add(len) else {
                    continue;
                };
                let Some(root_len) = data.checked_sub(root_off) else {
                    continue;
                };
                if root_len == 0 || !root_len.is_multiple_of(512) || root_len > 65_536 * 32 {
                    continue;
                }
                // Sectors full of end marks inside a table also match the signature: a real
                // pair starts with a media byte and has a directory at the root region start.
                let media = u32::from(ma != 0xFF) + u32::from(mb != 0xFF);
                pairs.push((a, b, len, root_off, root_len, media));
            }
        }
        pairs.sort_by_key(|p| std::cmp::Reverse((p.5, p.2)));
        let ctx = DirContext {
            fat32: false,
            max_cluster: 0xFFF6,
            max_size: u64::MAX,
            codepage: Codepage::Cp437,
        };
        let mut pair = None;
        for p in pairs.iter().take(64) {
            let mut sec = vec![0u8; 512];
            if r.read(p.3, &mut sec)?.is_clean() && dir::looks_like_dir(&sec, &ctx, true) {
                pair = Some((p.0, p.1, p.2, p.3, p.4));
                break;
            }
        }
        // For a lone survivor, a real table start (media byte) beats end-mark sectors.
        let survivor = small
            .iter()
            .filter(|h| h.1 != 0xFF)
            .map(|h| h.0)
            .max()
            .or_else(|| small.iter().map(|h| h.0).max());
        if let Some((a, b, len, root_off, root_len)) = pair {
            fats = vec![FatCopy { offset: a, len }, FatCopy { offset: b, len }];
            root = RootDir::Region {
                offset: root_off,
                len: root_len,
            };
            let count = (end - data) / u64::from(cluster);
            fs = Some(if count < 4085 {
                FsKind::Fat12
            } else {
                FsKind::Fat16
            });
        } else if let Some(b) = survivor
            && let Some(root_off) = root_region_start(r, b, data)?
        {
            // Only one copy survived (usually FAT2 after FAT1 was wiped): the root region
            // starts where directory-like sectors begin and run unbroken to the data area. The
            // lost first copy is kept in front so chains from the survivor read as the mirror.
            let len = root_off - b;
            fats = match b.checked_sub(len) {
                Some(a) => vec![FatCopy { offset: a, len }, FatCopy { offset: b, len }],
                None => vec![FatCopy { offset: b, len }],
            };
            root = RootDir::Region {
                offset: root_off,
                len: data - root_off,
            };
            let count = (end - data) / u64::from(cluster);
            fs = Some(if count < 4085 {
                FsKind::Fat12
            } else {
                FsKind::Fat16
            });
        }
    }
    // Last resort: a single FAT32 copy in front of the data area (FAT16 heads also pass the
    // FAT32 signature check, so this comes after every FAT12/16 reading).
    if fs.is_none()
        && let Some(&a) = f32s.iter().max()
    {
        fats = vec![FatCopy {
            offset: a,
            len: data - a,
        }];
        fs = Some(FsKind::Fat32);
    }
    for f in &fats {
        evidence.push(Evidence::FatSignature { offset: f.offset });
    }
    let count = (end - data) / u64::from(cluster);
    let fs = fs.unwrap_or(match wide {
        Some(true) => FsKind::Fat32,
        _ if count < 4085 => FsKind::Fat12,
        _ if count < 65_525 => FsKind::Fat16,
        _ => FsKind::Fat32,
    });
    let bits = match fs {
        FsKind::Fat12 => 12,
        FsKind::Fat16 => 16,
        _ => 32,
    };
    let mut max = count + 1;
    if let Some(f) = fats.first() {
        max = max.min((f.len * 8 / bits).saturating_sub(1));
    }
    let Ok(max_cluster) = u32::try_from(max.min(0x0FFF_FFF6)) else {
        return Ok(None);
    };
    if max_cluster < 2 {
        return Ok(None);
    }
    let mut g = FatGeometry {
        fs,
        volume_offset: None,
        bytes_per_sector: 512,
        cluster_size: cluster,
        data_offset: data,
        max_cluster,
        fats,
        root,
        label: None,
    };
    if fs == FsKind::Fat32 {
        // The root is cluster 2 on almost every FAT32 volume; trust it only if it looks right.
        let ctx = DirContext {
            fat32: true,
            max_cluster,
            max_size: end - data,
            codepage: Codepage::Cp437,
        };
        if let Some(off) = g.cluster_offset(2) {
            let mut b = vec![0u8; cluster as usize];
            if r.read(off, &mut b).is_ok_and(|rep| rep.is_clean())
                && dir::dir_start(&b, true).is_none()
                && dir::looks_like_dir(&b, &ctx, true)
            {
                g.root = RootDir::Cluster { cluster: 2 };
            }
        }
    }
    let score = 200 + 100 * g.fats.len() as u64;
    Ok(Some(Found {
        geometry: g,
        evidence,
        score,
    }))
}

/// First sector after `fat` from which every sector up to `data` looks like directory entries
/// (the first of them strictly so): the start of a FAT12/16 root region.
fn root_region_start(r: &GuardedReader, fat: u64, data: u64) -> Result<Option<u64>, GuardError> {
    let len = data.saturating_sub(fat);
    if !(1024..=64 << 20).contains(&len) {
        return Ok(None);
    }
    let mut buf = vec![0u8; usize::try_from(len).unwrap_or(0)];
    r.read(fat, &mut buf)?;
    let ctx = DirContext {
        fat32: false,
        max_cluster: 0xFFF6,
        max_size: u64::MAX,
        codepage: Codepage::Cp437,
    };
    let sectors: Vec<&[u8]> = buf.chunks(512).collect();
    let mut start = None;
    // Walk back from the data area while sectors stay directory-like.
    for (i, s) in sectors.iter().enumerate().rev() {
        if i == 0 || !dir::looks_like_dir(s, &ctx, false) {
            break;
        }
        if dir::looks_like_dir(s, &ctx, true) {
            start = Some(i);
        }
    }
    Ok(start.map(|i| fat + 512 * i as u64))
}

/// exFAT has no `.` entries, so subdirectory pointers inside directory entry sets vote instead:
/// for the right grid, each pointer lands on a sector that starts a directory.
fn exfat_from_votes(r: &GuardedReader, scan: &DeepScan) -> Result<Option<Found>, GuardError> {
    if scan.ex_dirs.len() < 2 {
        return Ok(None);
    }
    let ex = ExContext {
        max_cluster: u32::MAX - 1,
        cluster_size: 1,
        max_size: u64::MAX,
    };
    let mut pointers = Vec::new();
    for &o in scan.ex_dirs.iter().take(MAX_EX_DIRS) {
        let mut s = vec![0u8; 512];
        if !r.read(o, &mut s)?.is_clean() {
            continue;
        }
        let (entries, _) = exdir::parse_exdir(&s, &ex, 16);
        pointers.extend(
            entries
                .iter()
                .filter(|e| e.is_dir && e.first_cluster >= 2)
                .map(|e| e.first_cluster),
        );
        if pointers.len() >= MAX_EX_POINTERS {
            break;
        }
    }
    pointers.sort_unstable();
    pointers.dedup();
    if pointers.len() < 2 {
        return Ok(None);
    }
    let dirs: Vec<u64> = scan.ex_dirs.iter().take(MAX_EX_DIRS).copied().collect();
    // Directories are often allocated back to back, so grids shifted by a few clusters collect
    // votes too. Keep every strong grid and confirm it with the up-case table checksum.
    let mut grids: Vec<(u32, u64, u64)> = Vec::new();
    for shift in 9..=25u32 {
        let c = 1u64 << shift;
        let mut votes: BTreeMap<u64, u64> = BTreeMap::new();
        for &p in &pointers {
            let rel = u64::from(p - 2) * c;
            for &o in &dirs {
                if let Some(data) = o.checked_sub(rel) {
                    *votes.entry(data).or_default() += 1;
                }
            }
        }
        grids.extend(votes.into_iter().map(|(data, v)| (1u32 << shift, data, v)));
    }
    let top = grids.iter().map(|g| g.2).max().unwrap_or(0);
    grids.retain(|g| g.2 >= 2 && g.2 * 2 >= top && g.2 * 3 >= pointers.len() as u64);
    grids.sort_by_key(|g| std::cmp::Reverse(g.2));
    grids.truncate(256);
    let mut upcase = None;
    for &o in &scan.ex_roots {
        let mut s = vec![0u8; 512];
        if r.read(o, &mut s)?.is_clean()
            && let (_, info) = exdir::parse_exdir(&s, &ex, 16)
            && let Some(u) = info.upcase
        {
            upcase = Some((o, u));
            break;
        }
    }
    let mut chosen = None;
    if let Some((_, (uc, ulen, usum))) = upcase
        && (2..=1 << 20).contains(&ulen)
    {
        for &(cluster, data, v) in &grids {
            let Some(off) = u64::from(uc)
                .checked_sub(2)
                .map(|k| data + k * u64::from(cluster))
            else {
                continue;
            };
            let mut t = vec![0u8; usize::try_from(ulen).unwrap_or(0)];
            if r.read(off, &mut t).is_ok_and(|rep| rep.is_clean()) && exdir::upcase_checksum(&t) == usum {
                chosen = Some((cluster, data, v, true));
                break;
            }
        }
    }
    let Some((cluster, data, v, confirmed)) =
        chosen.or_else(|| grids.first().map(|&(c, d, v)| (c, d, v, false)))
    else {
        return Ok(None);
    };
    let end = scan.range.end;
    let mut max = (end.saturating_sub(data)) / u64::from(cluster) + 1;
    let mut root = RootDir::Unknown;
    let probe = FatGeometry {
        fs: FsKind::Exfat,
        volume_offset: None,
        bytes_per_sector: 512,
        cluster_size: cluster,
        data_offset: data,
        max_cluster: u32::try_from(max.min(0xFFFF_FFF5)).unwrap_or(0xFFFF_FFF5),
        fats: Vec::new(),
        root: RootDir::Unknown,
        label: None,
    };
    for &o in &scan.ex_roots {
        if let Some(c) = probe.cluster_at(o) {
            root = RootDir::Cluster { cluster: c };
            let mut s = vec![0u8; 512];
            if r.read(o, &mut s)?.is_clean() {
                let (_, info) = exdir::parse_exdir(&s, &ex, 16);
                if let Some((_, len)) = info.bitmap {
                    max = max.min(len * 8 + 1);
                }
            }
            break;
        }
    }
    let max_cluster = u32::try_from(max.min(0xFFFF_FFF5)).unwrap_or(0xFFFF_FFF5);
    let fats: Vec<FatCopy> = scan
        .fat_heads
        .iter()
        .filter(|h| h.exfat && h.offset < data)
        .map(|h| h.offset)
        .max()
        .map(|o| FatCopy {
            offset: o,
            len: (data - o).min((u64::from(max_cluster) + 1) * 4),
        })
        .into_iter()
        .collect();
    let mut evidence: Vec<Evidence> = fats
        .iter()
        .map(|f| Evidence::FatSignature { offset: f.offset })
        .collect();
    evidence.push(Evidence::DirectoryVotes { count: v });
    let g = FatGeometry {
        max_cluster,
        fats,
        root,
        ..probe
    };
    Ok(Some(Found {
        geometry: g,
        evidence,
        // An up-case checksum match pins the grid exactly; votes alone can be off by a few clusters.
        score: 300 + v.min(500) + if confirmed { 400 } else { 0 },
    }))
}

/// Directory seeds a deep scan found on this volume's cluster grid.
pub fn seeds_for(g: &FatGeometry, scan: &DeepScan) -> Vec<DirectorySeed> {
    let mut out = Vec::new();
    if g.is_exfat() {
        for &o in &scan.ex_dirs {
            if let Some(c) = g.cluster_at(o) {
                out.push(DirectorySeed {
                    cluster: u64::from(c),
                    parent: None,
                    origin: SeedOrigin::OwnScan,
                });
            }
        }
    } else {
        let fat32 = g.fs == FsKind::Fat32;
        for ds in &scan.dir_starts {
            let me = if fat32 { ds.me } else { u32::from(ds.me16) };
            let parent = if fat32 { ds.parent } else { u32::from(ds.parent16) };
            if g.cluster_at(ds.offset) == Some(me) {
                out.push(DirectorySeed {
                    cluster: u64::from(me),
                    parent: Some(u64::from(parent)),
                    origin: SeedOrigin::OwnScan,
                });
            }
        }
        // Fragments: directory clusters without `.`, parent unknown.
        for &o in &scan.dir_frags {
            if let Some(c) = g.cluster_at(o) {
                out.push(DirectorySeed {
                    cluster: u64::from(c),
                    parent: None,
                    origin: SeedOrigin::OwnScan,
                });
            }
        }
    }
    out
}
