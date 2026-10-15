//! Lost partition search: boot sectors anywhere on the Source, primary or backup, checked
//! against the volume they describe and compared with the partition table.
//!
//! Nothing here writes: the result says where volumes are and whether the table knows them.
//! Rebuilding a table is a separate, expert-mode decision.

use std::collections::BTreeMap;
use std::ops::Range;

use rescyo_fat::{FatGeometry, is_fat_head, parse_exfat_boot, parse_fat_boot};
use rescyo_fs::FsKind;
use rescyo_io::bytes::{u16_at, u64_at};
use rescyo_io::{GuardError, GuardedReader};
use rescyo_part::PartitionTable;
use serde::Serialize;

const SECTOR: usize = 512;
/// Boot sectors kept from one pass (images of disks stored as files can hold many).
const MAX_HITS: usize = 10_000;

/// How a found volume relates to the partition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PartitionStatus {
    /// The table has an entry starting here.
    Listed,
    /// Not in the table and not overlapping anything it lists: a lost partition.
    Lost,
    /// Not in the table but overlapping a listed partition (an older layout, or a damaged entry).
    Conflicting,
    /// Inside another found volume (a disk image stored as a file, for instance).
    Nested,
    /// The Source has no partition table and this volume starts at its first byte (a
    /// "superfloppy", as many USB sticks and memory cards are formatted).
    Unpartitioned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FoundPartition {
    pub offset: u64,
    /// Bytes from the start to the end of the volume (for NTFS including its backup boot sector).
    pub size: u64,
    pub fs: FsKind,
    pub label: Option<String>,
    /// The boot sector at the start was found and checked.
    pub primary_boot: bool,
    /// A backup boot sector was found and checked.
    pub backup_boot: bool,
    pub status: PartitionStatus,
    /// The table entry number, for listed partitions.
    pub number: Option<u32>,
}

/// Collects candidate boot sectors from a sequential pass.
#[derive(Debug, Default)]
pub struct BootScanner {
    hits: Vec<(u64, Box<[u8; SECTOR]>)>,
}

impl BootScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks at every 512-byte sector of `chunk`, which starts at absolute `at`.
    pub fn feed(&mut self, at: u64, chunk: &[u8]) {
        let skip = usize::try_from((SECTOR as u64 - at % SECTOR as u64) % SECTOR as u64).unwrap_or(0);
        let Some(aligned) = chunk.get(skip..) else { return };
        for (i, s) in aligned.as_chunks::<SECTOR>().0.iter().enumerate() {
            if self.hits.len() >= MAX_HITS {
                return;
            }
            let boot = s[510] == 0x55 && s[511] == 0xAA && looks_like_boot(s);
            if !boot && !is_ext_superblock(s) && !is_hfs_header(s) {
                continue;
            }
            self.hits.push((at + (skip + i * SECTOR) as u64, Box::new(*s)));
        }
    }

    /// Checks every hypothesis against the Source and compares the volumes with `table`.
    pub fn finish(
        self,
        r: &GuardedReader,
        table: &PartitionTable,
    ) -> Result<Vec<FoundPartition>, GuardError> {
        let disk = r.geometry().size;
        // (offset, fs) → found partition
        let mut found: BTreeMap<(u64, u8), FoundPartition> = BTreeMap::new();
        // Where confirmed volumes keep their backup boot sectors.
        let mut backup_sites: Vec<(u64, u8)> = Vec::new();
        for (at, s) in &self.hits {
            for (start, backup) in hypotheses(s, *at) {
                let Some(v) = volume(s, start) else { continue };
                if start.saturating_add(v.size) > disk.saturating_add(disk / 100 + (1 << 20)) {
                    continue;
                }
                if !v.check(r)? {
                    continue;
                }
                let e = found.entry((start, v.fs as u8)).or_insert(FoundPartition {
                    offset: start,
                    size: v.size,
                    fs: v.fs,
                    label: v.label.clone(),
                    primary_boot: false,
                    backup_boot: false,
                    status: PartitionStatus::Lost,
                    number: None,
                });
                if backup {
                    e.backup_boot = true;
                    backup_sites.push((*at, v.fs as u8));
                } else {
                    e.primary_boot = true;
                    e.label = v.label.clone().or(e.label.take());
                }
            }
        }
        // A backup copy also reads as a volume starting at its own sector; it is not one.
        found.retain(|k, f| f.backup_boot || !backup_sites.contains(k));
        let mut out: Vec<FoundPartition> = found.into_values().collect();
        classify(&mut out, table);
        Ok(out)
    }
}

/// The cheap first filter: a jump instruction or a known OEM name.
fn looks_like_boot(s: &[u8; SECTOR]) -> bool {
    matches!(&s[3..11], b"NTFS    " | b"EXFAT   ") || s[0] == 0xEB || s[0] == 0xE9
}

/// The first half of an ext2/3/4 superblock (everything the search needs lies in it).
fn is_ext_superblock(s: &[u8; SECTOR]) -> bool {
    s[56] == 0x53 && s[57] == 0xEF && rescyo_ext4::parse_superblock(s, 0).is_some()
}

/// An HFS+ or HFSX volume header (primary 1024 bytes in, backup 1024 bytes before the end).
fn is_hfs_header(s: &[u8; SECTOR]) -> bool {
    matches!(&s[0..2], b"H+" | b"HX") && rescyo_hfs::parse_header(s, 0).is_some()
}

/// Where a volume starts if `s` (at `at`) is its primary boot sector, or one of its backups.
fn hypotheses(s: &[u8; SECTOR], at: u64) -> Vec<(u64, bool)> {
    if is_hfs_header(s) {
        let mut h = Vec::new();
        if let Some(start) = at.checked_sub(rescyo_hfs::HEADER_OFFSET) {
            h.push((start, false));
        }
        if let Some(g) = rescyo_hfs::parse_header(s, 0)
            && let Some(start) = (at + rescyo_hfs::HEADER_OFFSET).checked_sub(g.size())
        {
            h.push((start, true));
        }
        return h;
    }
    if is_ext_superblock(s) {
        // A superblock names the block group it sits in: 0 for the primary (1 KiB into the
        // volume), the group's first block for a backup.
        let Some(g) = rescyo_ext4::parse_superblock(s, 0) else {
            return Vec::new();
        };
        let group = rescyo_ext4::superblock_group(s).unwrap_or(0);
        let start = if group == 0 {
            at.checked_sub(rescyo_ext4::SB_OFFSET)
        } else {
            (group * u64::from(g.blocks_per_group) + u64::from(g.first_data_block))
                .checked_mul(u64::from(g.block_size))
                .and_then(|n| at.checked_sub(n))
        };
        return start.map(|st| vec![(st, group != 0)]).unwrap_or_default();
    }
    let mut h = vec![(at, false)];
    let bps = u64::from(u16_at(s, 11).unwrap_or(0));
    match &s[3..11] {
        b"NTFS    " => {
            // The backup is the sector right after the volume's last sector.
            if let Some(total) = u64_at(s, 40)
                && let Some(start) = total.checked_mul(bps).and_then(|n| at.checked_sub(n))
            {
                h.push((start, true));
            }
        }
        b"EXFAT   " => {
            let bps = 1u64 << s[108].min(12);
            if let Some(start) = at.checked_sub(12 * bps) {
                h.push((start, true));
            }
        }
        _ => {
            // FAT32 keeps a backup at the sector its BPB names (usually 6).
            let backup = u64::from(u16_at(s, 50).unwrap_or(0));
            if u16_at(s, 22) == Some(0)
                && (1..32).contains(&backup)
                && let Some(start) = at.checked_sub(backup * bps)
            {
                h.push((start, true));
            }
        }
    }
    h
}

struct Volume {
    fs: FsKind,
    size: u64,
    label: Option<String>,
    fat: Option<FatGeometry>,
    ntfs: Option<rescyo_ntfs::NtfsGeometry>,
    ext4: Option<rescyo_ext4::Ext4Geometry>,
    hfs: Option<rescyo_hfs::HfsGeometry>,
}

/// The volume `s` describes if it starts at `start`.
fn volume(s: &[u8; SECTOR], start: u64) -> Option<Volume> {
    if is_hfs_header(s) {
        let g = rescyo_hfs::parse_header(s, start)?;
        return Some(Volume {
            fs: FsKind::HfsPlus,
            size: g.size(),
            label: None,
            fat: None,
            ntfs: None,
            ext4: None,
            hfs: Some(g),
        });
    }
    if is_ext_superblock(s) {
        let g = rescyo_ext4::parse_superblock(s, start)?;
        return Some(Volume {
            fs: FsKind::Ext4,
            size: g.size(),
            label: g.label.clone(),
            fat: None,
            ntfs: None,
            ext4: Some(g),
            hfs: None,
        });
    }
    if let Some(g) = rescyo_ntfs::parse_boot(s, start) {
        return Some(Volume {
            fs: FsKind::Ntfs,
            size: g.size + u64::from(g.bytes_per_sector),
            label: None,
            fat: None,
            ntfs: Some(g),
            ext4: None,
            hfs: None,
        });
    }
    if let Some(g) = parse_exfat_boot(s, start) {
        let bps = 1u64 << s[108];
        return Some(Volume {
            fs: FsKind::Exfat,
            size: u64_at(s, 72)? * bps,
            label: None,
            fat: Some(g),
            ntfs: None,
            ext4: None,
            hfs: None,
        });
    }
    let g = parse_fat_boot(s, start)?;
    let bps = u64::from(g.bytes_per_sector);
    let total = match u16_at(s, 19)? {
        0 => u64::from(rescyo_io::bytes::u32_at(s, 32)?),
        n => u64::from(n),
    };
    Some(Volume {
        fs: g.fs,
        size: total * bps,
        label: g.label.clone(),
        fat: Some(g),
        ntfs: None,
        ext4: None,
        hfs: None,
    })
}

impl Volume {
    /// Confirms the volume a boot sector describes: an allocation table, an exFAT root directory
    /// or an MFT record where the boot sector says (so a backup copy never confirms a volume at
    /// its own position).
    fn check(&self, r: &GuardedReader) -> Result<bool, GuardError> {
        let mut b = [0u8; 8];
        let probe = |at: u64, b: &mut [u8]| -> Result<bool, GuardError> {
            match r.read(at, b) {
                Ok(rep) => Ok(rep.is_clean()),
                Err(GuardError::OutOfBounds) => Ok(false),
                Err(e) => Err(e),
            }
        };
        if let Some(g) = &self.fat {
            for copy in &g.fats {
                if probe(copy.offset, &mut b)? && is_fat_head(&b, self.fs) {
                    return Ok(true);
                }
            }
            // exFAT often has a single FAT: its root directory, which must hold the allocation
            // bitmap (0x81) and up-case table (0x82) entries, confirms the start just as well.
            if self.fs == FsKind::Exfat
                && let rescyo_fat::RootDir::Cluster { cluster } = g.root
                && let Some(off) = g.cluster_offset(cluster)
            {
                let mut dir = vec![0u8; 32 * 16];
                if probe(off, &mut dir)? {
                    let types: Vec<u8> = dir.iter().step_by(32).copied().collect();
                    return Ok(types.contains(&0x81) && types.contains(&0x82));
                }
            }
            return Ok(false);
        }
        if let Some(g) = &self.ext4 {
            return rescyo_ext4::root_ok(r, g);
        }
        if let Some(g) = &self.hfs {
            // The catalog B-tree header node must be where the volume header says.
            let Some(off) = g.catalog.extents.first().and_then(|&(b, _)| g.block_offset(b)) else {
                return Ok(false);
            };
            let mut node = [0u8; 16];
            return Ok(probe(off, &mut node)? && node[8] == 1); // node kind 1: header node
        }
        if let Some(g) = &self.ntfs {
            for at in [Some(g.mft_offset), g.mftmirr_offset].into_iter().flatten() {
                if probe(at, &mut b)? && &b[..4] == b"FILE" {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Marks each found volume as listed, lost, conflicting or nested.
fn classify(found: &mut [FoundPartition], table: &PartitionTable) {
    let listed: Vec<(u32, Range<u64>)> = table
        .partitions
        .iter()
        .map(|p| (p.number, p.offset..p.offset + p.size))
        .collect();
    let ranges: Vec<Range<u64>> = found.iter().map(|f| f.offset..f.offset + f.size).collect();
    let primary: Vec<bool> = found.iter().map(|f| f.primary_boot).collect();
    for (i, f) in found.iter_mut().enumerate() {
        let me = f.offset..f.offset + f.size;
        if f.offset == 0 && table.kind == rescyo_part::TableKind::None {
            f.status = PartitionStatus::Unpartitioned;
        } else if let Some((n, _)) = listed.iter().find(|(_, r)| r.start == f.offset) {
            f.status = PartitionStatus::Listed;
            f.number = Some(*n);
        } else if ranges.iter().enumerate().any(|(j, r)| {
            j != i
                && r.start < me.start
                && me.end <= r.end
                && (primary.get(j).copied().unwrap_or(false) || !f.primary_boot)
        }) {
            f.status = PartitionStatus::Nested;
        } else if listed.iter().any(|(_, r)| overlaps(r, &me)) {
            f.status = PartitionStatus::Conflicting;
        } else {
            f.status = PartitionStatus::Lost;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rescyo_io::ReadPolicy;
    use rescyo_io::testing::MemSource;

    use super::*;

    /// A FAT16 volume of `sectors` sectors at `start`: boot sector, two FATs with their heads.
    fn fat16(img: &mut [u8], start: usize, sectors: u32) {
        let s = &mut img[start..start + 512];
        s[0] = 0xEB;
        s[1] = 0x3C;
        s[2] = 0x90;
        s[3..11].copy_from_slice(b"MSDOS5.0");
        s[11..13].copy_from_slice(&512u16.to_le_bytes());
        s[13] = 4; // sectors per cluster
        s[14..16].copy_from_slice(&4u16.to_le_bytes()); // reserved
        s[16] = 2;
        s[17..19].copy_from_slice(&512u16.to_le_bytes()); // root entries
        s[19..21].copy_from_slice(&0u16.to_le_bytes());
        s[21] = 0xF8;
        s[22..24].copy_from_slice(&100u16.to_le_bytes()); // sectors per FAT
        s[32..36].copy_from_slice(&sectors.to_le_bytes());
        s[510] = 0x55;
        s[511] = 0xAA;
        for fat in [start + 4 * 512, start + 104 * 512] {
            img[fat..fat + 4].copy_from_slice(&[0xF8, 0xFF, 0xFF, 0xFF]);
        }
    }

    fn reader(img: Vec<u8>) -> GuardedReader {
        GuardedReader::new(Arc::new(MemSource::new(img, 512)), ReadPolicy::default())
    }

    fn search(img: Vec<u8>) -> Vec<FoundPartition> {
        let r = reader(img.clone());
        let table = rescyo_part::read_partitions(&r).unwrap();
        let mut s = BootScanner::new();
        for (i, c) in img.chunks(100_000).enumerate() {
            s.feed(i as u64 * 100_000, c);
        }
        s.finish(&r, &table).unwrap()
    }

    #[test]
    fn finds_a_lost_partition_and_ignores_an_unchecked_boot_sector() {
        let mut img = vec![0u8; 40 << 20];
        fat16(&mut img, 1 << 20, 20_000 * 4 + 100);
        // A boot sector whose FATs are missing: not reported.
        fat16(&mut img, 30 << 20, 8000);
        img[(30 << 20) + 4 * 512] = 0;
        img[(30 << 20) + 104 * 512] = 0;
        let found = search(img);
        assert_eq!(found.len(), 1, "{found:?}");
        let f = &found[0];
        assert_eq!(
            (f.offset, f.fs, f.status),
            (1 << 20, FsKind::Fat16, PartitionStatus::Lost)
        );
        assert_eq!(f.size, u64::from(20_000u32 * 4 + 100) * 512);
        assert!(f.primary_boot && !f.backup_boot);
    }

    #[test]
    fn a_volume_inside_another_is_nested() {
        let mut img = vec![0u8; 64 << 20];
        fat16(&mut img, 1 << 20, 100_000);
        fat16(&mut img, 20 << 20, 10_000);
        let found = search(img);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].status, PartitionStatus::Lost);
        assert_eq!(found[1].status, PartitionStatus::Nested);
    }
}
