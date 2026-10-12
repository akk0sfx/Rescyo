//! Superblocks, volume geometry and group descriptors.

use rescyo_fs::{Evidence, FsKind, VolumeCandidate};
use rescyo_io::bytes::{slice_at, u16_at, u32_at};
use rescyo_io::{GuardError, GuardedReader};
use serde::{Deserialize, Serialize};

pub const MAGIC: u16 = 0xEF53;
/// Byte offset of the primary superblock inside the volume.
pub const SB_OFFSET: u64 = 1024;
pub const SB_LEN: usize = 1024;

const INCOMPAT_FILETYPE: u32 = 0x2;
const INCOMPAT_RECOVER: u32 = 0x4;
const INCOMPAT_JOURNAL_DEV: u32 = 0x8;
const INCOMPAT_META_BG: u32 = 0x10;
const INCOMPAT_EXTENTS: u32 = 0x40;
const INCOMPAT_64BIT: u32 = 0x80;
const INCOMPAT_MMP: u32 = 0x100;
const INCOMPAT_FLEX_BG: u32 = 0x200;
const INCOMPAT_EA_INODE: u32 = 0x400;
const INCOMPAT_DIRDATA: u32 = 0x1000;
const INCOMPAT_CSUM_SEED: u32 = 0x2000;
const INCOMPAT_LARGEDIR: u32 = 0x4000;
const INCOMPAT_INLINE_DATA: u32 = 0x8000;
const INCOMPAT_ENCRYPT: u32 = 0x10000;
const INCOMPAT_CASEFOLD: u32 = 0x20000;
/// Features whose on-disk layout this engine understands.
const UNDERSTOOD: u32 = INCOMPAT_FILETYPE
    | INCOMPAT_RECOVER
    | INCOMPAT_META_BG
    | INCOMPAT_EXTENTS
    | INCOMPAT_64BIT
    | INCOMPAT_MMP
    | INCOMPAT_FLEX_BG
    | INCOMPAT_EA_INODE
    | INCOMPAT_DIRDATA
    | INCOMPAT_CSUM_SEED
    | INCOMPAT_LARGEDIR
    | INCOMPAT_INLINE_DATA
    | INCOMPAT_ENCRYPT
    | INCOMPAT_CASEFOLD;
const RO_COMPAT_SPARSE_SUPER: u32 = 0x1;

/// Where everything is on an ext2/3/4 volume. Stored as a Volume candidate's `params`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ext4Geometry {
    /// Byte offset of the volume (block 0).
    pub volume_offset: u64,
    pub block_size: u32,
    pub blocks: u64,
    pub first_data_block: u32,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub inodes: u32,
    pub inode_size: u32,
    pub desc_size: u32,
    pub incompat: u32,
    pub ro_compat: u32,
    pub first_meta_bg: u32,
    pub label: Option<String>,
    /// The journal needed replaying when the volume was last written: recent changes may be
    /// missing.
    pub needs_recovery: bool,
}

impl Ext4Geometry {
    pub fn groups(&self) -> u64 {
        (self.blocks.saturating_sub(u64::from(self.first_data_block)))
            .div_ceil(u64::from(self.blocks_per_group.max(1)))
    }

    pub fn has_extents(&self) -> bool {
        self.incompat & INCOMPAT_EXTENTS != 0
    }

    pub fn inline_data(&self) -> bool {
        self.incompat & INCOMPAT_INLINE_DATA != 0
    }

    pub fn encrypted(&self) -> bool {
        self.incompat & INCOMPAT_ENCRYPT != 0
    }

    pub fn is_sane(&self) -> bool {
        self.block_size.is_power_of_two()
            && (1024..=65536).contains(&self.block_size)
            && self.inode_size.is_power_of_two()
            && (128..=self.block_size).contains(&self.inode_size)
            && self.inodes_per_group > 0
            && self.blocks_per_group > 0
            && self.blocks > u64::from(self.first_data_block)
            && self.desc_size >= 32
            && self.desc_size <= 1024
    }

    /// Absolute offset of block `b`, if inside the volume.
    pub fn block_offset(&self, b: u64) -> Option<u64> {
        (b < self.blocks).then(|| self.volume_offset + b * u64::from(self.block_size))
    }

    pub fn size(&self) -> u64 {
        self.blocks * u64::from(self.block_size)
    }

    /// Whether group `g` keeps a superblock backup (and descriptor copies).
    pub fn has_super(&self, g: u64) -> bool {
        if self.ro_compat & RO_COMPAT_SPARSE_SUPER == 0 || g <= 1 {
            return true;
        }
        [3u64, 5, 7].iter().any(|&p| {
            let mut x = p;
            while x < g {
                x *= p;
            }
            x == g
        })
    }

    pub fn to_candidate(&self, evidence: Vec<Evidence>, score: u64) -> VolumeCandidate {
        VolumeCandidate {
            fs: FsKind::Ext4,
            offset: Some(self.volume_offset),
            cluster_size: self.block_size,
            data_offset: self.volume_offset,
            size: Some(self.size()),
            label: self.label.clone(),
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

/// Parses a superblock (`s`, 1024 bytes) of a volume starting at `start`.
pub fn parse_superblock(s: &[u8], start: u64) -> Option<Ext4Geometry> {
    if u16_at(s, 56)? != MAGIC {
        return None;
    }
    let log = u32_at(s, 24)?;
    if log > 6 {
        return None;
    }
    let block_size = 1024u32 << log;
    let incompat = u32_at(s, 96)?;
    if incompat & !UNDERSTOOD != 0 || incompat & INCOMPAT_JOURNAL_DEV != 0 {
        return None;
    }
    let rev = u32_at(s, 76)?;
    let inode_size = if rev == 0 { 128 } else { u32::from(u16_at(s, 88)?) };
    let wide = incompat & INCOMPAT_64BIT != 0;
    let blocks_hi = if wide { u64::from(u32_at(s, 0x150)?) } else { 0 };
    let desc_size = if wide {
        u32::from(u16_at(s, 254)?).max(32)
    } else {
        32
    };
    let label = slice_at(s, 120, 16)
        .map(|b| String::from_utf8_lossy(b.split(|&c| c == 0).next().unwrap_or_default()).into_owned())
        .filter(|l| !l.is_empty());
    let g = Ext4Geometry {
        volume_offset: start,
        block_size,
        blocks: u64::from(u32_at(s, 4)?) | (blocks_hi << 32),
        first_data_block: u32_at(s, 20)?,
        blocks_per_group: u32_at(s, 32)?,
        inodes_per_group: u32_at(s, 40)?,
        inodes: u32_at(s, 0)?,
        inode_size,
        desc_size,
        incompat,
        ro_compat: u32_at(s, 100)?,
        first_meta_bg: u32_at(s, 0x104)?,
        label,
        needs_recovery: incompat & INCOMPAT_RECOVER != 0,
    };
    let fdb_ok = (block_size == 1024) == (g.first_data_block == 1) || g.first_data_block == 0;
    let counts_ok = u64::from(g.inodes) <= g.groups() * u64::from(g.inodes_per_group);
    (g.is_sane() && fdb_ok && counts_ok && g.blocks_per_group <= 8 * block_size).then_some(g)
}

/// The block group a superblock copy says it lives in (0 for the primary).
pub fn superblock_group(s: &[u8]) -> Option<u64> {
    u16_at(s, 90).map(u64::from)
}

/// A geometry found from superblocks, with what supports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub geometry: Ext4Geometry,
    pub evidence: Vec<Evidence>,
    pub score: u64,
}

/// Looks for the superblock of a volume starting at `offset`, falling back to the backup in
/// block group 1 for every block size.
pub fn probe(r: &GuardedReader, offset: u64) -> Result<Vec<Found>, GuardError> {
    let mut s = vec![0u8; SB_LEN];
    let read = |r: &GuardedReader, at: u64, s: &mut Vec<u8>| -> Result<bool, GuardError> {
        match r.read(at, s) {
            Ok(rep) => Ok(rep.is_clean()),
            Err(GuardError::OutOfBounds) => Ok(false),
            Err(e) => Err(e),
        }
    };
    if read(r, offset + SB_OFFSET, &mut s)?
        && let Some(g) = parse_superblock(&s, offset)
    {
        return Ok(vec![Found {
            geometry: g,
            evidence: vec![Evidence::BootSector {
                offset: offset + SB_OFFSET,
            }],
            score: 1000,
        }]);
    }
    // Group 1 starts at block blocks_per_group (+1 with 1 KiB blocks). mkfs picks 8 × block
    // size blocks per group unless told otherwise; smaller groups are tried too.
    for log in 0..=2u32 {
        let block = 1024u64 << log;
        let mut tried = Vec::new();
        for bpg in [8 * block, 32768, 16384, 8192, 4096, 2048, 1024] {
            if bpg > 8 * block || tried.contains(&bpg) {
                continue;
            }
            tried.push(bpg);
            let at = offset + (bpg + u64::from(log == 0)) * block;
            if read(r, at, &mut s)?
                && superblock_group(&s) == Some(1)
                && let Some(g) = parse_superblock(&s, offset)
                && u64::from(g.block_size) == block
                && u64::from(g.blocks_per_group) == bpg
            {
                return Ok(vec![Found {
                    geometry: g,
                    evidence: vec![Evidence::BackupBootSector { offset: at }],
                    score: 900,
                }]);
            }
        }
    }
    Ok(Vec::new())
}

/// Absolute block numbers of every group's inode table, from the group descriptors.
pub fn inode_tables(r: &GuardedReader, g: &Ext4Geometry) -> Result<Vec<Option<u64>>, GuardError> {
    let groups = g.groups();
    let bs = u64::from(g.block_size);
    let per_block = bs / u64::from(g.desc_size);
    let desc_blocks = groups.div_ceil(per_block);
    let mut out = Vec::with_capacity(usize::try_from(groups.min(1 << 24)).unwrap_or(0));
    let mut block = vec![0u8; usize::try_from(bs).unwrap_or(4096)];
    for db in 0..desc_blocks {
        if out.len() as u64 >= groups.min(1 << 24) {
            break;
        }
        let meta = g.incompat & INCOMPAT_META_BG != 0 && db >= u64::from(g.first_meta_bg);
        let at = if meta {
            // META_BG: descriptor block db lives in the first group of meta group db.
            let first = db * per_block;
            first * u64::from(g.blocks_per_group)
                + u64::from(g.first_data_block)
                + u64::from(g.has_super(first))
        } else {
            u64::from(g.first_data_block) + 1 + db
        };
        let readable = match g.block_offset(at) {
            Some(off) => match r.read(off, &mut block) {
                Ok(rep) => rep.is_clean(),
                Err(GuardError::OutOfBounds) => false,
                Err(e) => return Err(e),
            },
            None => false,
        };
        for i in 0..per_block {
            if out.len() as u64 >= groups {
                break;
            }
            let d = usize::try_from(i * u64::from(g.desc_size)).unwrap_or(usize::MAX);
            let lo = u32_at(&block, d + 8).map(u64::from);
            let hi = if g.desc_size >= 64 {
                u32_at(&block, d + 0x28).map(u64::from)
            } else {
                Some(0)
            };
            let t = match (readable, lo, hi) {
                (true, Some(lo), Some(hi)) => Some(lo | (hi << 32)).filter(|&t| t > 0 && t < g.blocks),
                _ => None,
            };
            out.push(t);
        }
    }
    Ok(out)
}
