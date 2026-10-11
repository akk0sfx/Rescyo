//! NTFS boot sectors and volume geometry.

use rescyo_fs::{Evidence, FsKind, VolumeCandidate};
use rescyo_io::bytes::{slice_at, u8_at, u16_at, u64_at};
use rescyo_io::{GuardError, GuardedReader};
use serde::{Deserialize, Serialize};

use crate::record::{record_size, signed_at};

/// Where everything is on an NTFS volume. Stored as a Volume candidate's `params`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NtfsGeometry {
    /// Byte offset of the volume (cluster 0).
    pub volume_offset: u64,
    pub bytes_per_sector: u32,
    pub cluster_size: u32,
    /// Volume size in bytes (may be unknown when inferred: then the Source end).
    pub size: u64,
    /// Absolute byte offset of the MFT's first record, as recorded or inferred.
    pub mft_offset: u64,
    /// Absolute byte offset of $MFTMirr, when known.
    pub mftmirr_offset: Option<u64>,
    pub record_size: u32,
}

impl NtfsGeometry {
    pub fn is_sane(&self) -> bool {
        self.cluster_size.is_power_of_two()
            && (512..=(2 << 20)).contains(&self.cluster_size)
            && matches!(self.bytes_per_sector, 512 | 1024 | 2048 | 4096)
            && self.record_size.is_power_of_two()
            && (512..=65536).contains(&self.record_size)
            && self.mft_offset >= self.volume_offset
    }

    /// Absolute offset of logical cluster `lcn`.
    pub fn lcn_offset(&self, lcn: u64) -> Option<u64> {
        lcn.checked_mul(u64::from(self.cluster_size))?
            .checked_add(self.volume_offset)
    }

    pub fn to_candidate(&self, evidence: Vec<Evidence>, score: u64) -> VolumeCandidate {
        VolumeCandidate {
            fs: FsKind::Ntfs,
            offset: Some(self.volume_offset),
            cluster_size: self.cluster_size,
            data_offset: self.volume_offset,
            size: Some(self.size),
            label: None,
            evidence,
            score,
            params: serde_json::to_value(self).unwrap_or(serde_json::Value::Null),
        }
    }

    pub fn from_candidate(c: &VolumeCandidate) -> Option<Self> {
        let g: Self = serde_json::from_value(c.params.clone()).ok()?;
        g.is_sane().then_some(g)
    }
}

/// A geometry with what supports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub geometry: NtfsGeometry,
    pub evidence: Vec<Evidence>,
    pub score: u64,
}

/// Parses an NTFS boot sector describing a volume that starts at `at`.
pub fn parse_boot(s: &[u8], at: u64) -> Option<NtfsGeometry> {
    if slice_at(s, 3, 8)? != b"NTFS    " || u16_at(s, 510)? != 0xAA55 {
        return None;
    }
    let bps = u32::from(u16_at(s, 11)?);
    let spc_raw = u8_at(s, 13)?;
    let spc: u32 = if spc_raw > 0x80 {
        1u32.checked_shl(256 - u32::from(spc_raw))?
    } else {
        u32::from(spc_raw)
    };
    if !matches!(bps, 512 | 1024 | 2048 | 4096) || spc == 0 || !spc.is_power_of_two() {
        return None;
    }
    let cluster = bps.checked_mul(spc)?;
    let total = u64_at(s, 40)?;
    let mft = u64_at(s, 48)?;
    let mirr = u64_at(s, 56)?;
    let c = u64::from(cluster);
    let size = total.checked_mul(u64::from(bps))?;
    let rs = record_size(signed_at(s, 64)?, c)?;
    if mft.checked_mul(c)? >= size || mirr.checked_mul(c)? >= size {
        return None;
    }
    let g = NtfsGeometry {
        volume_offset: at,
        bytes_per_sector: bps,
        cluster_size: cluster,
        size,
        mft_offset: at + mft * c,
        mftmirr_offset: Some(at + mirr * c),
        record_size: u32::try_from(rs).ok()?,
    };
    g.is_sane().then_some(g)
}

/// Looks for an NTFS boot sector at `offset`, else its backup in the last sector of the
/// volume (`end` is the end of the partition, when known).
pub fn probe_boot(r: &GuardedReader, offset: u64, end: Option<u64>) -> Result<Vec<Found>, GuardError> {
    let mut s = vec![0u8; 512];
    let clean = |r: &GuardedReader, at: u64, s: &mut Vec<u8>| -> Result<bool, GuardError> {
        match r.read(at, s) {
            Ok(rep) => Ok(rep.is_clean()),
            Err(GuardError::OutOfBounds) => Ok(false),
            Err(e) => Err(e),
        }
    };
    if clean(r, offset, &mut s)?
        && let Some(g) = parse_boot(&s, offset)
    {
        return Ok(vec![Found {
            geometry: g,
            evidence: vec![Evidence::BootSector { offset }],
            score: 1000,
        }]);
    }
    if let Some(end) = end
        && let Some(at) = end.checked_sub(512)
        && clean(r, at, &mut s)?
        && let Some(g) = parse_boot(&s, offset)
        && offset + g.size == at
    {
        return Ok(vec![Found {
            geometry: g,
            evidence: vec![Evidence::BackupBootSector { offset: at }],
            score: 900,
        }]);
    }
    Ok(Vec::new())
}
