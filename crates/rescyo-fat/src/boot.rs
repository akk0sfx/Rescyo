//! Boot sectors: FAT12/16/32 BIOS parameter blocks and the exFAT boot region.

use rescyo_fs::{Evidence, FsKind};
use rescyo_io::bytes::{slice_at, u8_at, u16_at, u32_at, u64_at};
use rescyo_io::{GuardError, GuardedReader};

use crate::geometry::{FatCopy, FatGeometry, RootDir};

/// A geometry found from boot sectors, with what supports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub geometry: FatGeometry,
    pub evidence: Vec<Evidence>,
    pub score: u64,
}

/// Parses a FAT12/16/32 boot sector found at absolute byte `at`.
pub fn parse_fat_boot(s: &[u8], at: u64) -> Option<FatGeometry> {
    if u16_at(s, 510)? != 0xAA55 {
        return None;
    }
    let bps = u32::from(u16_at(s, 11)?);
    let spc = u32::from(u8_at(s, 13)?);
    let rsv = u64::from(u16_at(s, 14)?);
    let nfats = u64::from(u8_at(s, 16)?);
    let root_ents = u64::from(u16_at(s, 17)?);
    let tot16 = u64::from(u16_at(s, 19)?);
    let media = u8_at(s, 21)?;
    let fatsz16 = u64::from(u16_at(s, 22)?);
    let tot32 = u64::from(u32_at(s, 32)?);
    let fatsz32 = u64::from(u32_at(s, 36)?);
    if !matches!(bps, 512 | 1024 | 2048 | 4096) || !spc.is_power_of_two() || spc > 128 {
        return None;
    }
    if rsv == 0 || !(1..=2).contains(&nfats) || !(media == 0xF0 || media >= 0xF8) {
        return None;
    }
    let cluster = bps * spc;
    if cluster > 256 * 1024 {
        return None;
    }
    let fatsz = if fatsz16 != 0 { fatsz16 } else { fatsz32 };
    let totsec = if tot16 != 0 { tot16 } else { tot32 };
    if fatsz == 0 || totsec == 0 {
        return None;
    }
    let bps64 = u64::from(bps);
    let root_sectors = (root_ents * 32).div_ceil(bps64);
    let meta = rsv + nfats * fatsz + root_sectors;
    let data_sectors = totsec.checked_sub(meta)?;
    let count = data_sectors / u64::from(spc);
    let fs = if count < 4085 {
        FsKind::Fat12
    } else if count < 65525 {
        FsKind::Fat16
    } else {
        FsKind::Fat32
    };
    // FAT32 has no fixed root region and a 32-bit FAT size; FAT12/16 the reverse.
    if (fs == FsKind::Fat32) != (fatsz16 == 0) || (fs == FsKind::Fat32 && root_ents != 0) {
        return None;
    }
    // The FAT must be large enough to describe every cluster.
    let bits = match fs {
        FsKind::Fat12 => 12,
        FsKind::Fat16 => 16,
        _ => 32,
    };
    if fatsz * bps64 * 8 / bits < count + 2 {
        return None;
    }
    let fat1 = at + rsv * bps64;
    let fat_len = fatsz * bps64;
    let fats = (0..nfats)
        .map(|i| FatCopy {
            offset: fat1 + i * fat_len,
            len: fat_len,
        })
        .collect();
    let root_region = fat1 + nfats * fat_len;
    let data_offset = root_region + root_sectors * bps64;
    let max_cluster = u32::try_from(count + 1).ok()?;
    let (root, label_at, ext_sig_at) = if fs == FsKind::Fat32 {
        let rc = u32_at(s, 44)?;
        if rc < 2 || rc > max_cluster {
            return None;
        }
        (RootDir::Cluster { cluster: rc }, 71, 66)
    } else {
        (
            RootDir::Region {
                offset: root_region,
                len: root_ents * 32,
            },
            43,
            38,
        )
    };
    let label = (u8_at(s, ext_sig_at) == Some(0x29))
        .then(|| slice_at(s, label_at, 11))
        .flatten()
        .map(|b| String::from_utf8_lossy(b).trim_end().to_owned())
        .filter(|l| !l.is_empty() && l != "NO NAME");
    Some(FatGeometry {
        fs,
        volume_offset: Some(at),
        bytes_per_sector: bps,
        cluster_size: cluster,
        data_offset,
        max_cluster,
        fats,
        root,
        label,
    })
}

/// Parses an exFAT main or backup boot sector describing a volume that starts at `at`.
pub fn parse_exfat_boot(s: &[u8], at: u64) -> Option<FatGeometry> {
    if slice_at(s, 3, 8)? != b"EXFAT   " || u16_at(s, 510)? != 0xAA55 {
        return None;
    }
    if slice_at(s, 11, 53)?.iter().any(|&b| b != 0) {
        return None;
    }
    let vol_len = u64_at(s, 72)?;
    let fat_off = u64::from(u32_at(s, 80)?);
    let fat_len = u64::from(u32_at(s, 84)?);
    let heap = u64::from(u32_at(s, 88)?);
    let count = u32_at(s, 92)?;
    let root = u32_at(s, 96)?;
    let bps_shift = u32::from(u8_at(s, 108)?);
    let spc_shift = u32::from(u8_at(s, 109)?);
    let nfats = u64::from(u8_at(s, 110)?);
    if !(9..=12).contains(&bps_shift) || bps_shift + spc_shift > 25 || !(1..=2).contains(&nfats) {
        return None;
    }
    if fat_off < 24 || heap < fat_off + nfats * fat_len || count == 0 || count > 0xFFFF_FFF5 {
        return None;
    }
    let max_cluster = count.checked_add(1)?;
    if root < 2 || root > max_cluster || fat_len * (1 << bps_shift) / 4 < u64::from(count) + 2 {
        return None;
    }
    let bps = 1u32 << bps_shift;
    let cluster = bps << spc_shift;
    let b = u64::from(bps);
    if heap + u64::from(count) * u64::from(cluster) / b > vol_len {
        return None;
    }
    let fats = (0..nfats)
        .map(|i| FatCopy {
            offset: at + (fat_off + i * fat_len) * b,
            len: fat_len * b,
        })
        .collect();
    Some(FatGeometry {
        fs: FsKind::Exfat,
        volume_offset: Some(at),
        bytes_per_sector: bps,
        cluster_size: cluster,
        data_offset: at + heap * b,
        max_cluster,
        fats,
        root: RootDir::Cluster { cluster: root },
        label: None,
    })
}

/// Verifies the exFAT boot region checksum: `region` holds sectors 0..=11 of a boot region.
pub fn exfat_checksum_ok(region: &[u8], bps: usize) -> bool {
    let Some(body) = slice_at(region, 0, bps * 11) else {
        return false;
    };
    let mut sum: u32 = 0;
    for (i, &b) in body.iter().enumerate() {
        if i == 106 || i == 107 || i == 112 {
            continue;
        }
        sum = sum.rotate_right(1).wrapping_add(u32::from(b));
    }
    let Some(sector) = slice_at(region, bps * 11, bps) else {
        return false;
    };
    sector.as_chunks::<4>().0.iter().all(|c| *c == sum.to_le_bytes())
}

/// Looks for a FAT or exFAT boot sector for a volume starting at `offset`, falling back to the
/// backup copies (FAT32: sector 6; exFAT: sector 12) when the primary is unusable.
pub fn probe_boot(r: &GuardedReader, offset: u64) -> Result<Vec<Found>, GuardError> {
    let mut found = Vec::new();
    if let Some(f) = boot_at(r, offset, offset, false)? {
        found.push(f);
    }
    if found.is_empty() {
        for bps in [512u64, 4096] {
            for backup in [6u64, 12] {
                let Some(at) = offset.checked_add(backup * bps) else {
                    continue;
                };
                if let Some(f) = boot_at(r, at, offset, true)?
                    && !found.iter().any(|x: &Found| x.geometry == f.geometry)
                {
                    found.push(f);
                }
            }
        }
    }
    Ok(found)
}

fn boot_at(r: &GuardedReader, at: u64, volume: u64, backup: bool) -> Result<Option<Found>, GuardError> {
    let mut s = vec![0u8; 512];
    match r.read(at, &mut s) {
        Ok(rep) if rep.is_clean() => {}
        Ok(_) | Err(GuardError::OutOfBounds) => return Ok(None),
        Err(e) => return Err(e),
    }
    let ev = if backup {
        Evidence::BackupBootSector { offset: at }
    } else {
        Evidence::BootSector { offset: at }
    };
    if let Some(g) = parse_fat_boot(&s, volume) {
        if backup && at != volume + 6 * u64::from(g.bytes_per_sector) {
            return Ok(None);
        }
        let fat_ok = fat_signature_ok(r, &g)?;
        let score = if backup { 900 } else { 1000 } + if fat_ok { 100 } else { 0 };
        let mut evidence = vec![ev];
        if fat_ok && let Some(f) = g.fats.first() {
            evidence.push(Evidence::FatSignature { offset: f.offset });
        }
        return Ok(Some(Found {
            geometry: g,
            evidence,
            score,
        }));
    }
    if let Some(g) = parse_exfat_boot(&s, volume) {
        let bps = g.bytes_per_sector as usize;
        if backup && at != volume + 12 * bps as u64 {
            return Ok(None);
        }
        let mut region = vec![0u8; bps * 12];
        let checksum = match r.read(at, &mut region) {
            Ok(rep) if rep.is_clean() => exfat_checksum_ok(&region, bps),
            Ok(_) | Err(GuardError::OutOfBounds) => false,
            Err(e) => return Err(e),
        };
        let score = if backup { 900 } else { 1000 } + if checksum { 100 } else { 0 };
        return Ok(Some(Found {
            geometry: g,
            evidence: vec![ev],
            score,
        }));
    }
    Ok(None)
}

/// Whether the first FAT entry holds the media signature (F8 FF.. or the FAT12/16 forms).
fn fat_signature_ok(r: &GuardedReader, g: &FatGeometry) -> Result<bool, GuardError> {
    let Some(f) = g.fats.first() else { return Ok(false) };
    let mut b = [0u8; 8];
    match r.read(f.offset, &mut b) {
        Ok(rep) if rep.is_clean() => Ok(crate::table::is_fat_head(&b, g.fs)),
        Ok(_) | Err(GuardError::OutOfBounds) => Ok(false),
        Err(e) => Err(e),
    }
}
