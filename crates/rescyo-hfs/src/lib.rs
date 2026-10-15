//! HFS+ and HFSX recovery engine (experimental, ADR-0005), written from Apple's published
//! format description (Technical Note TN1150).
//!
//! The volume header (or its backup) locates the catalog and extents-overflow B-trees. The
//! catalog's leaf chain gives every folder and file; data forks are assembled from the eight
//! extents of the catalog record plus the overflow tree. Deleted records are looked for in the
//! free space of catalog leaf nodes, where a deletion leaves the shifted-out bytes. Names are
//! stored decomposed (NFD) and are reported composed (NFC). Every structure is hostile input:
//! node counts, record counts, depths and sizes are bounded.
#![forbid(unsafe_code)]

mod btree;
mod walk;

use rescyo_fs::{Evidence, FsKind, VolumeCandidate};
use rescyo_io::{GuardError, GuardedReader};
use serde::{Deserialize, Serialize};

pub use walk::{WalkError, WalkStats, walk};

/// Byte offset of the volume header inside the volume (the backup is 1024 bytes from the end).
pub const HEADER_OFFSET: u64 = 1024;

pub(crate) fn be16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

pub(crate) fn be32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

pub(crate) fn be64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// A fork: its size and first eight extents (start block, block count).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fork {
    pub size: u64,
    pub total_blocks: u32,
    pub extents: Vec<(u32, u32)>,
}

impl Fork {
    pub(crate) fn parse(b: &[u8], o: usize) -> Option<Self> {
        let size = be64(b, o)?;
        let total_blocks = be32(b, o + 12)?;
        let extents = (0..8)
            .filter_map(|i| {
                let e = o + 16 + i * 8;
                Some((be32(b, e)?, be32(b, e + 4)?))
            })
            .filter(|&(_, n)| n > 0)
            .collect();
        Some(Self {
            size,
            total_blocks,
            extents,
        })
    }
}

/// Where everything is on an HFS+ volume. Stored as a Volume candidate's `params`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HfsGeometry {
    pub volume_offset: u64,
    pub block_size: u32,
    pub total_blocks: u32,
    /// HFSX: names compare case-sensitively.
    pub hfsx: bool,
    pub catalog: Fork,
    pub extents_file: Fork,
    pub files: u32,
    pub folders: u32,
}

impl HfsGeometry {
    pub fn size(&self) -> u64 {
        u64::from(self.total_blocks) * u64::from(self.block_size)
    }

    pub fn is_sane(&self) -> bool {
        self.block_size.is_power_of_two()
            && (512..=(1 << 20)).contains(&self.block_size)
            && self.total_blocks > 0
    }

    /// Absolute offset of allocation block `b`.
    pub fn block_offset(&self, b: u32) -> Option<u64> {
        (b < self.total_blocks).then(|| self.volume_offset + u64::from(b) * u64::from(self.block_size))
    }

    pub fn to_candidate(&self, evidence: Vec<Evidence>, score: u64) -> VolumeCandidate {
        VolumeCandidate {
            fs: FsKind::HfsPlus,
            offset: Some(self.volume_offset),
            cluster_size: self.block_size,
            data_offset: self.volume_offset,
            size: Some(self.size()),
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

/// Parses a volume header (`h`, at least 512 bytes) of a volume starting at `start`.
pub fn parse_header(h: &[u8], start: u64) -> Option<HfsGeometry> {
    let sig = h.get(0..2)?;
    let hfsx = match sig {
        b"H+" => false,
        b"HX" => true,
        _ => return None,
    };
    let version = be16(h, 2)?;
    if !(4..=5).contains(&version) {
        return None;
    }
    let g = HfsGeometry {
        volume_offset: start,
        block_size: be32(h, 40)?,
        total_blocks: be32(h, 44)?,
        hfsx,
        catalog: Fork::parse(h, 272)?,
        extents_file: Fork::parse(h, 192)?,
        files: be32(h, 32)?,
        folders: be32(h, 36)?,
    };
    let fits = |f: &Fork| {
        f.extents
            .iter()
            .all(|&(s, n)| u64::from(s) + u64::from(n) <= u64::from(g.total_blocks))
    };
    (g.is_sane() && !g.catalog.extents.is_empty() && fits(&g.catalog) && fits(&g.extents_file)).then_some(g)
}

/// A geometry found from volume headers, with what supports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub geometry: HfsGeometry,
    pub evidence: Vec<Evidence>,
    pub score: u64,
}

fn read_clean(r: &GuardedReader, at: u64, buf: &mut [u8]) -> Result<bool, GuardError> {
    match r.read(at, buf) {
        Ok(rep) => Ok(rep.is_clean()),
        Err(GuardError::OutOfBounds) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Looks for the volume header at `offset`, else its backup 1024 bytes before `end`.
pub fn probe(r: &GuardedReader, offset: u64, end: Option<u64>) -> Result<Vec<Found>, GuardError> {
    let mut h = vec![0u8; 512];
    if read_clean(r, offset + HEADER_OFFSET, &mut h)?
        && let Some(g) = parse_header(&h, offset)
    {
        return Ok(vec![Found {
            geometry: g,
            evidence: vec![Evidence::BootSector {
                offset: offset + HEADER_OFFSET,
            }],
            score: 1000,
        }]);
    }
    if let Some(at) = end.and_then(|e| e.checked_sub(HEADER_OFFSET))
        && read_clean(r, at, &mut h)?
        && let Some(g) = parse_header(&h, offset)
        && offset + g.size() == at + HEADER_OFFSET
    {
        return Ok(vec![Found {
            geometry: g,
            evidence: vec![Evidence::BackupBootSector { offset: at }],
            score: 900,
        }]);
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests;
