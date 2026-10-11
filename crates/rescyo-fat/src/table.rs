//! Allocation table access with a bounded page cache.

use std::collections::HashMap;
use std::ops::Range;

use rescyo_fs::FsKind;
use rescyo_io::{GuardError, GuardedReader};

use crate::geometry::FatCopy;

const PAGE: u64 = 64 * 1024;
const MAX_PAGES: usize = 1024; // 64 MiB of table cached at most

/// What one allocation-table cell says about a cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Free,
    Next(u32),
    End,
    Bad,
    /// A value no valid table contains (reserved, out of range, self-link).
    Invalid,
    /// The cell lies in an unreadable sector or outside the table.
    Unknown,
}

#[derive(Debug)]
struct Page {
    data: Vec<u8>,
    unreadable: Vec<Range<usize>>,
}

/// One copy of the allocation table.
#[derive(Debug)]
pub struct FatTable {
    copy: FatCopy,
    fs: FsKind,
    max_cluster: u32,
    pages: HashMap<u64, Page>,
}

impl FatTable {
    pub fn new(copy: FatCopy, fs: FsKind, max_cluster: u32) -> Self {
        Self {
            copy,
            fs,
            max_cluster,
            pages: HashMap::new(),
        }
    }

    /// The link stored for `cluster`.
    pub fn link(&mut self, r: &GuardedReader, cluster: u32) -> Result<Link, GuardError> {
        if cluster < 2 || cluster > self.max_cluster {
            return Ok(Link::Invalid);
        }
        let c = u64::from(cluster);
        let raw = match self.fs {
            FsKind::Fat12 => {
                let Some(v) = self.read_le(r, c * 3 / 2, 2)? else {
                    return Ok(Link::Unknown);
                };
                if cluster & 1 == 1 { v >> 4 } else { v & 0xFFF }
            }
            FsKind::Fat16 => match self.read_le(r, c * 2, 2)? {
                Some(v) => v,
                None => return Ok(Link::Unknown),
            },
            FsKind::Fat32 => match self.read_le(r, c * 4, 4)? {
                Some(v) => v & 0x0FFF_FFFF,
                None => return Ok(Link::Unknown),
            },
            FsKind::Exfat => match self.read_le(r, c * 4, 4)? {
                Some(v) => v,
                None => return Ok(Link::Unknown),
            },
            FsKind::Ntfs => return Ok(Link::Invalid),
        };
        Ok(self.decode(cluster, raw))
    }

    fn decode(&self, cluster: u32, v: u32) -> Link {
        let (bad, end) = match self.fs {
            FsKind::Fat12 => (0xFF7, 0xFF8),
            FsKind::Fat16 => (0xFFF7, 0xFFF8),
            FsKind::Fat32 => (0x0FFF_FFF7, 0x0FFF_FFF8),
            _ => (0xFFFF_FFF7, 0xFFFF_FFFF),
        };
        match v {
            0 => Link::Free,
            v if v == bad => Link::Bad,
            v if v >= end => Link::End,
            v if v >= 2 && v <= self.max_cluster && v != cluster => Link::Next(v),
            _ => Link::Invalid,
        }
    }

    /// Reads `n` (2 or 4) bytes at `off` within the table as little-endian.
    fn read_le(&mut self, r: &GuardedReader, off: u64, n: u64) -> Result<Option<u32>, GuardError> {
        if off + n > self.copy.len {
            return Ok(None);
        }
        let mut v: u32 = 0;
        for i in 0..n {
            let at = off + i;
            let Some(b) = self.byte(r, at)? else {
                return Ok(None);
            };
            v |= u32::from(b) << (8 * i);
        }
        Ok(Some(v))
    }

    fn byte(&mut self, r: &GuardedReader, at: u64) -> Result<Option<u8>, GuardError> {
        let page_no = at / PAGE;
        if !self.pages.contains_key(&page_no) {
            if self.pages.len() >= MAX_PAGES {
                self.pages.clear();
            }
            let start = page_no * PAGE;
            let len = usize::try_from(PAGE.min(self.copy.len - start)).unwrap_or(0);
            let mut data = vec![0u8; len];
            let unreadable = match r.read(self.copy.offset + start, &mut data) {
                Ok(rep) => rep
                    .unreadable
                    .iter()
                    .filter_map(|u| {
                        let a = usize::try_from(u.start - (self.copy.offset + start)).ok()?;
                        let b = usize::try_from(u.end - (self.copy.offset + start)).ok()?;
                        Some(a..b)
                    })
                    .collect(),
                Err(GuardError::OutOfBounds) => std::iter::once(0..len).collect(),
                Err(e) => return Err(e),
            };
            self.pages.insert(page_no, Page { data, unreadable });
        }
        let Some(page) = self.pages.get(&page_no) else {
            return Ok(None);
        };
        let i = usize::try_from(at % PAGE).unwrap_or(usize::MAX);
        if page.unreadable.iter().any(|u| u.contains(&i)) {
            return Ok(None);
        }
        Ok(page.data.get(i).copied())
    }
}

/// Whether `b` starts like an allocation table of the given kind: media byte, then end marks.
pub fn is_fat_head(b: &[u8], fs: FsKind) -> bool {
    let (Some(&m), Some(&b1), Some(&b2)) = (b.first(), b.get(1), b.get(2)) else {
        return false;
    };
    let media = m == 0xF0 || m >= 0xF8;
    match fs {
        FsKind::Fat12 => media && b1 == 0xFF && b2 == 0xFF,
        FsKind::Fat16 => media && b1 == 0xFF && b2 == 0xFF && b.get(3).is_some_and(|&x| x | 0xC0 == 0xFF),
        FsKind::Fat32 => {
            media
                && b1 == 0xFF
                && b2 == 0xFF
                && b.get(3).is_some_and(|&x| x & 0x0F == 0x0F)
                && b.get(4..8)
                    .and_then(|e| <[u8; 4]>::try_from(e).ok())
                    .is_some_and(|e| u32::from_le_bytes(e) & 0x0FFF_FFFF >= 0x0FFF_FFF8)
        }
        FsKind::Exfat => b.get(0..8) == Some(&[0xF8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF][..]),
        FsKind::Ntfs => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rescyo_io::testing::{FaultySource, MemSource};
    use rescyo_io::{ReadPolicy, ReadPolicy as P};
    use std::sync::Arc;

    fn reader(img: Vec<u8>) -> GuardedReader {
        GuardedReader::new(Arc::new(MemSource::new(img, 512)), ReadPolicy::default())
    }

    #[test]
    fn decodes_fat12_packed_entries() {
        // Entries: [F8F, FFF, 003, 004, FFF, 000]
        let mut img = vec![0u8; 4096];
        img[..9].copy_from_slice(&[0xF8, 0xFF, 0xFF, 0x03, 0x40, 0x00, 0xFF, 0x0F, 0x00]);
        let r = reader(img);
        let mut t = FatTable::new(FatCopy { offset: 0, len: 512 }, FsKind::Fat12, 300);
        assert_eq!(t.link(&r, 2).unwrap(), Link::Next(3));
        assert_eq!(t.link(&r, 3).unwrap(), Link::Next(4));
        assert_eq!(t.link(&r, 4).unwrap(), Link::End);
        assert_eq!(t.link(&r, 5).unwrap(), Link::Free);
    }

    #[test]
    fn decodes_fat32_masks_high_bits_and_rejects_self_links() {
        let mut img = vec![0u8; 4096];
        let put =
            |img: &mut Vec<u8>, c: usize, v: u32| img[c * 4..c * 4 + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut img, 2, 0xF000_0003); // high nibble is reserved and ignored
        put(&mut img, 3, 0x0FFF_FFFF);
        put(&mut img, 4, 4); // self link
        put(&mut img, 5, 0x0FFF_FFF7);
        put(&mut img, 6, 999_999); // beyond max cluster
        let r = reader(img);
        let mut t = FatTable::new(FatCopy { offset: 0, len: 4096 }, FsKind::Fat32, 1000);
        assert_eq!(t.link(&r, 2).unwrap(), Link::Next(3));
        assert_eq!(t.link(&r, 3).unwrap(), Link::End);
        assert_eq!(t.link(&r, 4).unwrap(), Link::Invalid);
        assert_eq!(t.link(&r, 5).unwrap(), Link::Bad);
        assert_eq!(t.link(&r, 6).unwrap(), Link::Invalid);
        assert_eq!(t.link(&r, 1).unwrap(), Link::Invalid);
        assert_eq!(t.link(&r, 1001).unwrap(), Link::Invalid);
    }

    #[test]
    fn unreadable_table_sectors_are_unknown_not_free() {
        let src = FaultySource::new(MemSource::new(vec![0u8; 8192], 512)).bad(1..2);
        let r = GuardedReader::new(
            Arc::new(src),
            P {
                alarm_after_errors: None,
                ..P::default()
            },
        );
        let mut t = FatTable::new(FatCopy { offset: 0, len: 4096 }, FsKind::Fat32, 2000);
        assert_eq!(t.link(&r, 130).unwrap(), Link::Unknown); // byte 520 is in sector 1
        assert_eq!(t.link(&r, 2).unwrap(), Link::Free);
    }

    #[test]
    fn recognises_table_heads() {
        assert!(is_fat_head(
            &[0xF8, 0xFF, 0xFF, 0x0F, 0xFF, 0xFF, 0xFF, 0x0F],
            FsKind::Fat32
        ));
        assert!(!is_fat_head(
            &[0xF8, 0xFF, 0xFF, 0x0F, 0x00, 0x00, 0x00, 0x00],
            FsKind::Fat32
        ));
        assert!(is_fat_head(&[0xF8, 0xFF, 0xFF, 0xFF], FsKind::Fat16));
        assert!(is_fat_head(&[0xF0, 0xFF, 0xFF], FsKind::Fat12));
        assert!(!is_fat_head(&[0x00, 0xFF, 0xFF], FsKind::Fat12));
        assert!(is_fat_head(
            &[0xF8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
            FsKind::Exfat
        ));
    }
}
