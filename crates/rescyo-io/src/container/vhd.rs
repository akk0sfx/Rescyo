//! VHD (Virtual Hard Disk Image Format Specification, Microsoft, 2006): fixed, dynamic and
//! differencing disks. All fields are big-endian.
//!
//! A 512-byte footer ends the file; dynamic and differencing disks also keep a copy of it at
//! offset 0, which is used when the end of the file is damaged or truncated.

use std::path::Path;
use std::sync::Mutex;

use super::{
    ContainerFormat, HostFile, Layout, OpenError, Opened, ParentRef, Piece, dir_of,
    foreign_path_candidates,
};
use crate::ReadError;
use crate::bytes::{be16_at, be32_at, be64_at, bytes_at, slice_at};

const F: ContainerFormat = ContainerFormat::Vhd;
const SECTOR: u64 = 512;
const UNALLOCATED: u32 = 0xFFFF_FFFF;
/// Largest block size accepted (the specification's default is 2 MiB).
const MAX_BLOCK: u32 = 256 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiskType {
    Fixed,
    Dynamic,
    Differencing,
}

#[derive(Debug, Clone)]
struct Footer {
    data_offset: u64,
    current_size: u64,
    disk_type: DiskType,
    unique_id: [u8; 16],
    checksum_ok: bool,
}

fn checksum(buf: &[u8], skip: std::ops::Range<usize>) -> u32 {
    let sum = buf
        .iter()
        .enumerate()
        .filter(|(i, _)| !skip.contains(i))
        .fold(0u32, |acc, (_, &b)| acc.wrapping_add(u32::from(b)));
    !sum
}

fn parse_footer(b: &[u8]) -> Option<Footer> {
    if slice_at(b, 0, 8)? != b"conectix" {
        return None;
    }
    let disk_type = match be32_at(b, 60)? {
        2 => DiskType::Fixed,
        3 => DiskType::Dynamic,
        4 => DiskType::Differencing,
        _ => return None,
    };
    Some(Footer {
        data_offset: be64_at(b, 16)?,
        current_size: be64_at(b, 48)?,
        disk_type,
        unique_id: bytes_at(b, 68)?,
        checksum_ok: be32_at(b, 64)? == checksum(slice_at(b, 0, 512)?, 64..68),
    })
}

pub(super) fn open(path: &Path) -> Result<Opened, OpenError> {
    let host = HostFile::open(path)?;
    let mut notes = Vec::new();
    let tail = if host.len >= SECTOR {
        parse_footer(&host.read_vec(host.len - SECTOR, 512)?)
    } else {
        None
    };
    let head = if host.len >= SECTOR {
        parse_footer(&host.read_vec(0, 512)?)
    } else {
        None
    };
    // Prefer the end footer; fall back to the copy at the start (dynamic disks only) and say so.
    let footer = match (tail, head) {
        (Some(t), Some(h)) if !t.checksum_ok && h.checksum_ok => {
            notes.push("footer at the end of the file is damaged; used the copy at the start".into());
            h
        }
        (Some(t), _) => t,
        (None, Some(h)) => {
            notes.push("footer at the end of the file is missing; used the copy at the start".into());
            h
        }
        (None, None) => return Err(OpenError::invalid(F, "no VHD footer found")),
    };
    if !footer.checksum_ok {
        notes.push("footer checksum does not match".into());
    }
    match footer.disk_type {
        DiskType::Fixed => {
            if host.len < footer.current_size {
                notes.push(format!(
                    "file is truncated: {} of {} bytes present; the rest is unreadable",
                    host.len, footer.current_size
                ));
            }
            Ok(Opened {
                files: vec![host],
                layout: Box::new(FixedLayout {
                    size: footer.current_size,
                }),
                logical_sector: 512,
                parent: None,
                notes,
            })
        }
        DiskType::Dynamic | DiskType::Differencing => open_sparse(path, host, footer, notes),
    }
}

fn open_sparse(
    path: &Path,
    host: HostFile,
    footer: Footer,
    mut notes: Vec<String>,
) -> Result<Opened, OpenError> {
    let h = host
        .read_vec(footer.data_offset, 1024)
        .map_err(|_| OpenError::invalid(F, "dynamic disk header is outside the file"))?;
    if slice_at(&h, 0, 8) != Some(b"cxsparse".as_slice()) {
        return Err(OpenError::invalid(F, "dynamic disk header signature is wrong"));
    }
    let field = |v: Option<u64>| v.ok_or_else(|| OpenError::invalid(F, "short dynamic header"));
    if be32_at(&h, 36) != Some(checksum(&h, 36..40)) {
        notes.push("dynamic disk header checksum does not match".into());
    }
    let table_offset = field(be64_at(&h, 16))?;
    let entries = field(be32_at(&h, 28).map(u64::from))?;
    let block_size = field(be32_at(&h, 32).map(u64::from))?;
    if !block_size.is_power_of_two() || block_size < SECTOR || block_size > u64::from(MAX_BLOCK) {
        return Err(OpenError::invalid(F, format!("implausible block size {block_size}")));
    }
    let needed = footer.current_size.div_ceil(block_size);
    if entries < needed {
        notes.push(format!(
            "block table has {entries} entries for {needed} blocks; the rest reads as zeros"
        ));
    }
    // Never trust the entry count further than the virtual size needs or the file can hold.
    let entries = entries.min(needed).min(host.len / 4);
    let bat_bytes = usize::try_from(entries * 4).map_err(|_| OpenError::invalid(F, "huge table"))?;
    let raw = host
        .read_vec(table_offset, bat_bytes)
        .map_err(|_| OpenError::invalid(F, "block allocation table is outside the file"))?;
    let bat: Vec<u32> = raw
        .chunks_exact(4)
        .filter_map(|c| be32_at(c, 0))
        .collect();
    let bitmap = (block_size / SECTOR).div_ceil(8).next_multiple_of(SECTOR);

    let differencing = footer.disk_type == DiskType::Differencing;
    let parent = if differencing {
        Some(parent_ref(path, &host, &h)?)
    } else {
        None
    };
    Ok(Opened {
        files: vec![host],
        layout: Box::new(SparseLayout {
            size: footer.current_size,
            block_size,
            bitmap,
            bat,
            differencing,
            bitmaps: Mutex::new(Vec::new()),
        }),
        logical_sector: 512,
        parent,
        notes,
    })
}

/// Parent locators: Windows relative (`W2ru`) and absolute (`W2ku`) UTF-16LE paths, a macOS
/// file URL (`MacX`), then the parent's name stored in the header, next to the child.
fn parent_ref(path: &Path, host: &HostFile, h: &[u8]) -> Result<ParentRef, OpenError> {
    let dir = dir_of(path);
    let mut candidates = Vec::new();
    let mut by_priority: Vec<(u8, String)> = Vec::new();
    for i in 0..8usize {
        let e = 576 + i * 24;
        let (Some(code), Some(len), Some(off)) =
            (be32_at(h, e), be32_at(h, e + 8), be64_at(h, e + 16))
        else {
            continue;
        };
        let rank = match &code.to_be_bytes() {
            b"W2ru" => 0,
            b"W2ku" => 1,
            b"MacX" => 2,
            _ => continue,
        };
        if len == 0 || len > 4096 {
            continue;
        }
        let Ok(bytes) = host.read_vec(off, len as usize) else {
            continue;
        };
        let text = if rank == 2 {
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            utf16le(&bytes)
        };
        by_priority.push((rank, text));
    }
    by_priority.sort_by_key(|(r, _)| *r);
    for (_, text) in by_priority {
        for c in foreign_path_candidates(&dir, &text) {
            if !candidates.contains(&c) {
                candidates.push(c);
            }
        }
    }
    // The header also stores the parent's file name (UTF-16BE, 512 bytes).
    if let Some(name) = slice_at(h, 64, 512) {
        let units: Vec<u16> = name
            .chunks_exact(2)
            .filter_map(|c| be16_at(c, 0))
            .take_while(|&u| u != 0)
            .collect();
        let name = String::from_utf16_lossy(&units);
        for c in foreign_path_candidates(&dir, &name) {
            if !candidates.contains(&c) {
                candidates.push(c);
            }
        }
    }
    let want: [u8; 16] = bytes_at(h, 40).unwrap_or([0; 16]);
    Ok(ParentRef {
        candidates,
        check: Some(Box::new(move |info| {
            let Some(parent) = info.files.first() else {
                return Ok(());
            };
            let id = read_unique_id(parent).unwrap_or([0; 16]);
            if id == want || want == [0; 16] {
                Ok(())
            } else {
                Err(format!(
                    "parent {} has a different identifier than the child expects; data may be inconsistent",
                    parent.display()
                ))
            }
        })),
    })
}

fn read_unique_id(path: &Path) -> Option<[u8; 16]> {
    let host = HostFile::open(path).ok()?;
    let tail = host.read_vec(host.len.checked_sub(SECTOR)?, 512).ok()?;
    parse_footer(&tail)
        .or_else(|| parse_footer(&host.read_vec(0, 512).ok()?))
        .map(|f| f.unique_id)
}

fn utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c.first().copied().unwrap_or(0), c.get(1).copied().unwrap_or(0)]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

#[derive(Debug)]
struct FixedLayout {
    size: u64,
}

impl Layout for FixedLayout {
    fn size(&self) -> u64 {
        self.size
    }

    fn locate(&self, _: &[HostFile], offset: u64) -> Result<(Piece, u64), ReadError> {
        Ok((Piece::Data { file: 0, offset }, self.size - offset))
    }
}

#[derive(Debug)]
struct SparseLayout {
    size: u64,
    block_size: u64,
    /// Bytes of sector bitmap before each block's data.
    bitmap: u64,
    bat: Vec<u32>,
    differencing: bool,
    /// Recently read sector bitmaps of a differencing disk: (block, bitmap).
    bitmaps: Mutex<Vec<(u64, Vec<u8>)>>,
}

impl SparseLayout {
    fn bitmap_of(&self, files: &[HostFile], block: u64, at: u64) -> Result<Vec<u8>, ReadError> {
        if let Ok(cache) = self.bitmaps.lock()
            && let Some((_, b)) = cache.iter().find(|(blk, _)| *blk == block)
        {
            return Ok(b.clone());
        }
        let mut b = vec![0u8; usize::try_from(self.bitmap).map_err(|_| ReadError::Media)?];
        files.first().ok_or(ReadError::Media)?.read(at, &mut b)?;
        if let Ok(mut cache) = self.bitmaps.lock() {
            if cache.len() >= 16 {
                cache.remove(0);
            }
            cache.push((block, b.clone()));
        }
        Ok(b)
    }
}

impl Layout for SparseLayout {
    fn size(&self) -> u64 {
        self.size
    }

    fn locate(&self, files: &[HostFile], offset: u64) -> Result<(Piece, u64), ReadError> {
        let block = offset / self.block_size;
        let within = offset % self.block_size;
        let run = (self.block_size - within).min(self.size - offset);
        let unallocated = if self.differencing {
            Piece::Parent
        } else {
            Piece::Zero
        };
        let entry = usize::try_from(block)
            .ok()
            .and_then(|i| self.bat.get(i).copied())
            .unwrap_or(UNALLOCATED);
        if entry == UNALLOCATED {
            return Ok((unallocated, run));
        }
        let bitmap_at = u64::from(entry) * SECTOR;
        let data_at = bitmap_at + self.bitmap;
        if !self.differencing {
            // A dynamic disk's bitmap only says which sectors were written; unwritten sectors
            // of an allocated block hold zeros on disk, so the block is read as stored.
            return Ok((
                Piece::Data {
                    file: 0,
                    offset: data_at + within,
                },
                run,
            ));
        }
        // Differencing: each bitmap bit says whether the sector is here (1) or in the parent.
        let bitmap = self.bitmap_of(files, block, bitmap_at)?;
        let first = within / SECTOR;
        let last = (within + run) / SECTOR;
        let bit = |s: u64| -> bool {
            usize::try_from(s / 8)
                .ok()
                .and_then(|i| bitmap.get(i))
                .is_some_and(|byte| byte & (0x80 >> (s % 8)) != 0)
        };
        let here = bit(first);
        let same = (first..last).take_while(|&s| bit(s) == here).count() as u64;
        let len = (same * SECTOR).min(run).max(SECTOR.min(run));
        if here {
            Ok((
                Piece::Data {
                    file: 0,
                    offset: data_at + within,
                },
                len,
            ))
        } else {
            Ok((Piece::Parent, len))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::container::testutil::{file_in, read_all};
    use crate::container::{ContainerFormat, open_image};
    use crate::testing::pattern;

    /// A footer as the specification lays it out.
    pub fn footer(disk_type: u32, size: u64, data_offset: u64, id: u8) -> Vec<u8> {
        let mut f = vec![0u8; 512];
        f[0..8].copy_from_slice(b"conectix");
        f[8..12].copy_from_slice(&2u32.to_be_bytes());
        f[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        f[16..24].copy_from_slice(&data_offset.to_be_bytes());
        f[40..48].copy_from_slice(&size.to_be_bytes());
        f[48..56].copy_from_slice(&size.to_be_bytes());
        f[60..64].copy_from_slice(&disk_type.to_be_bytes());
        f[68..84].copy_from_slice(&[id; 16]);
        let c = checksum(&f, 64..68);
        f[64..68].copy_from_slice(&c.to_be_bytes());
        f
    }

    /// A dynamic (3) or differencing (4) disk with 4 KiB blocks: `blocks[i]` is the data of
    /// block i, or None when unallocated. Returns the file bytes.
    pub fn sparse(
        disk_type: u32,
        size: u64,
        blocks: &[Option<Vec<u8>>],
        bitmaps: &[Vec<u8>],
        parent_name: &str,
        parent_id: u8,
    ) -> Vec<u8> {
        let bs = 4096u64;
        let entries = size.div_ceil(bs);
        let mut file = footer(disk_type, size, 512, 7);
        let mut h = vec![0u8; 1024];
        h[0..8].copy_from_slice(b"cxsparse");
        h[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
        h[16..24].copy_from_slice(&1536u64.to_be_bytes());
        h[24..28].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        h[28..32].copy_from_slice(&u32::try_from(entries).unwrap().to_be_bytes());
        h[32..36].copy_from_slice(&u32::try_from(bs).unwrap().to_be_bytes());
        h[40..56].copy_from_slice(&[parent_id; 16]);
        for (i, u) in parent_name.encode_utf16().enumerate() {
            h[64 + i * 2..66 + i * 2].copy_from_slice(&u.to_be_bytes());
        }
        let c = checksum(&h, 36..40);
        h[36..40].copy_from_slice(&c.to_be_bytes());
        file.extend_from_slice(&h);
        let bat_len = (entries * 4).next_multiple_of(512);
        let mut bat = vec![0xFFu8; usize::try_from(bat_len).unwrap()];
        let mut data = Vec::new();
        let mut next_sector = (1536 + bat_len) / 512;
        for (i, b) in blocks.iter().enumerate() {
            if let Some(b) = b {
                bat[i * 4..i * 4 + 4].copy_from_slice(&u32::try_from(next_sector).unwrap().to_be_bytes());
                let mut bm = bitmaps.get(i).cloned().unwrap_or_else(|| vec![0xFF; 1]);
                bm.resize(512, 0);
                data.extend_from_slice(&bm);
                data.extend_from_slice(b);
                next_sector += (512 + bs) / 512;
            }
        }
        file.extend_from_slice(&bat);
        file.extend_from_slice(&data);
        file.extend_from_slice(&footer(disk_type, size, 512, 7));
        file
    }

    #[test]
    fn fixed_disk_reads_its_data_and_ignores_the_footer() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = pattern(8192);
        bytes.extend_from_slice(&footer(2, 8192, u64::MAX, 1));
        let p = file_in(dir.path(), "fixed.vhd", &bytes);
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert_eq!(info.format, ContainerFormat::Vhd);
        assert!(info.notes.is_empty(), "{:?}", info.notes);
        assert_eq!(read_all(src.as_ref()), pattern(8192));
    }

    #[test]
    fn dynamic_disk_zero_fills_unallocated_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let b1 = pattern(4096);
        let bytes = sparse(3, 12288, &[None, Some(b1.clone()), None], &[], "", 0);
        let p = file_in(dir.path(), "dyn.vhd", &bytes);
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert!(info.notes.is_empty(), "{:?}", info.notes);
        let all = read_all(src.as_ref());
        assert_eq!(&all[..4096], &[0u8; 4096][..]);
        assert_eq!(&all[4096..8192], &b1[..]);
        assert_eq!(&all[8192..], &[0u8; 4096][..]);
    }

    #[test]
    fn damaged_end_footer_falls_back_to_the_start_copy() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = sparse(3, 4096, &[Some(pattern(4096))], &[], "", 0);
        let n = bytes.len();
        bytes[n - 500] ^= 0xFF;
        let p = file_in(dir.path(), "dyn.vhd", &bytes);
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert!(info.notes.iter().any(|n| n.contains("copy at the start")), "{:?}", info.notes);
        assert_eq!(read_all(src.as_ref()), pattern(4096));
    }

    #[test]
    fn block_pointing_past_the_end_is_unreadable_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = sparse(3, 8192, &[Some(pattern(4096)), Some(pattern(4096))], &[], "", 0);
        // Point block 1 far past the end of the file.
        bytes[1536 + 4..1536 + 8].copy_from_slice(&0x00FF_FFFFu32.to_be_bytes());
        let p = file_in(dir.path(), "dyn.vhd", &bytes);
        let (src, _) = open_image(&p, 512, None).unwrap();
        let mut ok = vec![0u8; 4096];
        src.read_at(0, &mut ok).unwrap();
        assert_eq!(ok, pattern(4096));
        let mut bad = vec![0u8; 512];
        assert!(matches!(src.read_at(4096, &mut bad), Err(ReadError::Media)));
    }

    #[test]
    fn differencing_disk_reads_unset_sectors_from_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        // Parent: two blocks of pattern data, unique id 7 (set by `sparse`).
        let parent = sparse(3, 8192, &[Some(pattern(4096)), Some(pattern(4096))], &[], "", 0);
        file_in(dir.path(), "base.vhd", &parent);
        // Child: block 0 present with only sectors 0 and 1 marked as written (bitmap 0b1100_0000).
        let child_block = vec![0xABu8; 4096];
        let child = sparse(4, 8192, &[Some(child_block), None], &[vec![0xC0]], "base.vhd", 7);
        let p = file_in(dir.path(), "child.vhd", &child);
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert!(info.notes.is_empty(), "{:?}", info.notes);
        assert_eq!(info.files.len(), 2);
        let all = read_all(src.as_ref());
        assert_eq!(&all[..1024], &[0xABu8; 1024][..]);
        assert_eq!(&all[1024..4096], &pattern(4096)[1024..]);
        assert_eq!(&all[4096..], &pattern(4096)[..]);
    }

    #[test]
    fn missing_parent_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let child = sparse(4, 4096, &[None], &[], "gone.vhd", 7);
        let p = file_in(dir.path(), "child.vhd", &child);
        let err = open_image(&p, 512, None).unwrap_err();
        assert!(matches!(err, OpenError::ParentMissing { .. }), "{err}");
    }

    #[test]
    fn hostile_headers_do_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = sparse(3, 8192, &[Some(pattern(4096))], &[], "", 0);
        // Absurd entry count and block size.
        bytes[512 + 28..512 + 32].copy_from_slice(&u32::MAX.to_be_bytes());
        let p = file_in(dir.path(), "a.vhd", &bytes);
        let _ = open_image(&p, 512, None);
        bytes[512 + 32..512 + 36].copy_from_slice(&3u32.to_be_bytes());
        let p = file_in(dir.path(), "b.vhd", &bytes);
        assert!(open_image(&p, 512, None).is_err());
    }
}
