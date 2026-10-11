//! qcow2 (QEMU copy-on-write v2 and v3, `docs/interop/qcow2.txt` in QEMU). Fields are
//! big-endian. A two-level table (L1 → L2) maps every cluster of the virtual disk to a host
//! cluster, to a compressed run of bytes, to zeros, or to the backing file.
//!
//! Reads the current state of the disk; internal snapshots are left alone.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::{
    ContainerFormat, HostFile, Layout, OpenError, Opened, ParentRef, Piece, dir_of,
    foreign_path_candidates, inflate_raw,
};
use crate::ReadError;
use crate::bytes::{be32_at, be64_at, slice_at};

const F: ContainerFormat = ContainerFormat::Qcow2;
const MAGIC: &[u8] = b"QFI\xfb";
/// Offset bits of L1 and standard L2 entries (bits 9–55).
const OFFSET_MASK: u64 = 0x00FF_FFFF_FFFF_FE00;
const COMPRESSED: u64 = 1 << 62;
/// Standard L2 entry flag (v3): the cluster reads as zeros.
const ZERO_FLAG: u64 = 1;

const INCOMPAT_DIRTY: u64 = 1 << 0;
const INCOMPAT_CORRUPT: u64 = 1 << 1;
const INCOMPAT_DATA_FILE: u64 = 1 << 2;
const INCOMPAT_COMPRESSION: u64 = 1 << 3;
const INCOMPAT_EXTL2: u64 = 1 << 4;

pub(super) fn open(path: &Path) -> Result<Opened, OpenError> {
    let host = HostFile::open(path)?;
    let h = host
        .read_vec(0, 112.min(usize::try_from(host.len).unwrap_or(112)))
        .map_err(|_| OpenError::invalid(F, "header is truncated"))?;
    if slice_at(&h, 0, 4) != Some(MAGIC) {
        return Err(OpenError::invalid(F, "bad magic"));
    }
    let field = |v: Option<u64>, what: &str| {
        v.ok_or_else(|| OpenError::invalid(F, format!("header too short for {what}")))
    };
    let version = field(be32_at(&h, 4).map(u64::from), "version")?;
    if version != 2 && version != 3 {
        return Err(OpenError::unsupported(F, format!("version {version}")));
    }
    let backing_offset = field(be64_at(&h, 8), "backing file")?;
    let backing_size = field(be32_at(&h, 16).map(u64::from), "backing file")?;
    let cluster_bits = field(be32_at(&h, 20).map(u64::from), "cluster size")?;
    let size = field(be64_at(&h, 24), "size")?;
    let crypt = field(be32_at(&h, 32).map(u64::from), "encryption")?;
    let l1_size = field(be32_at(&h, 36).map(u64::from), "L1 size")?;
    let l1_offset = field(be64_at(&h, 40), "L1 offset")?;
    if !(9..=21).contains(&cluster_bits) {
        return Err(OpenError::invalid(F, format!("cluster_bits {cluster_bits}")));
    }
    if crypt != 0 {
        return Err(OpenError::unsupported(F, "encrypted images"));
    }
    let mut notes = Vec::new();
    let mut zstd = false;
    if version == 3 {
        let incompat = field(be64_at(&h, 72), "features")?;
        let header_len = field(be32_at(&h, 100).map(u64::from), "header length")?;
        if incompat & INCOMPAT_DATA_FILE != 0 {
            return Err(OpenError::unsupported(F, "an external data file"));
        }
        if incompat & INCOMPAT_EXTL2 != 0 {
            return Err(OpenError::unsupported(F, "extended L2 entries (subclusters)"));
        }
        let known = INCOMPAT_DIRTY | INCOMPAT_CORRUPT | INCOMPAT_COMPRESSION;
        if incompat & !known != 0 {
            return Err(OpenError::unsupported(
                F,
                format!("incompatible features {:#x}", incompat & !known),
            ));
        }
        if incompat & INCOMPAT_DIRTY != 0 {
            notes.push("image was not closed cleanly (dirty); its tables are read as they are".into());
        }
        if incompat & INCOMPAT_CORRUPT != 0 {
            notes.push("QEMU marked this image as corrupt; damaged clusters will be unreadable".into());
        }
        if incompat & INCOMPAT_COMPRESSION != 0 && header_len > 104 {
            zstd = h.get(104).copied() == Some(1);
        }
    }
    if zstd {
        notes.push("compressed clusters use zstd, which is not supported yet: they are unreadable".into());
    }

    let cluster = 1u64 << cluster_bits;
    let l2_entries = cluster / 8;
    let needed = size.div_ceil(cluster).div_ceil(l2_entries);
    if l1_size < needed {
        notes.push(format!("L1 table has {l1_size} entries for {needed}; the rest reads as unallocated"));
    }
    let l1_len = l1_size.min(needed).min(host.len / 8);
    let raw = host
        .read_vec(l1_offset, usize::try_from(l1_len * 8).map_err(|_| OpenError::invalid(F, "huge L1"))?)
        .map_err(|_| OpenError::invalid(F, "L1 table is outside the file"))?;
    let l1: Vec<u64> = raw.chunks_exact(8).filter_map(|c| be64_at(c, 0)).collect();

    let parent = if backing_offset != 0 && backing_size != 0 {
        let len = usize::try_from(backing_size.min(1023)).unwrap_or(1023);
        let name = host
            .read_vec(backing_offset, len)
            .map_err(|_| OpenError::invalid(F, "backing file name is outside the file"))?;
        let name = String::from_utf8_lossy(&name).into_owned();
        Some(ParentRef {
            candidates: foreign_path_candidates(&dir_of(path), &name),
            check: None,
        })
    } else {
        None
    };
    let backed = parent.is_some();
    Ok(Opened {
        files: vec![host],
        layout: Box::new(Qcow2Layout {
            size,
            cluster_bits,
            l1,
            backed,
            zstd,
            l2_cache: Mutex::new(Vec::new()),
        }),
        logical_sector: 512,
        parent,
        notes,
    })
}

#[derive(Debug)]
struct Qcow2Layout {
    size: u64,
    cluster_bits: u64,
    l1: Vec<u64>,
    backed: bool,
    zstd: bool,
    l2_cache: Mutex<Vec<(u64, Arc<Vec<u64>>)>>,
}

impl Qcow2Layout {
    fn cluster(&self) -> u64 {
        1 << self.cluster_bits
    }

    fn l2(&self, files: &[HostFile], at: u64) -> Result<Arc<Vec<u64>>, ReadError> {
        if let Ok(c) = self.l2_cache.lock()
            && let Some((_, t)) = c.iter().find(|(o, _)| *o == at)
        {
            return Ok(Arc::clone(t));
        }
        if at % self.cluster() != 0 {
            return Err(ReadError::Media);
        }
        let mut raw = vec![0u8; usize::try_from(self.cluster()).map_err(|_| ReadError::Media)?];
        files.first().ok_or(ReadError::Media)?.read(at, &mut raw)?;
        let t = Arc::new(raw.chunks_exact(8).filter_map(|c| be64_at(c, 0)).collect::<Vec<_>>());
        if let Ok(mut c) = self.l2_cache.lock() {
            if c.len() >= 32 {
                c.remove(0);
            }
            c.push((at, Arc::clone(&t)));
        }
        Ok(t)
    }

    /// Host offset and byte length of a compressed cluster's data.
    fn compressed_span(&self, entry: u64) -> (u64, u64) {
        let x = 62 - (self.cluster_bits - 8);
        let offset = entry & ((1u64 << x) - 1);
        let sectors = ((entry >> x) & ((1u64 << (self.cluster_bits - 8)) - 1)) + 1;
        (offset, (sectors * 512).saturating_sub(offset & 511))
    }
}

impl Layout for Qcow2Layout {
    fn size(&self) -> u64 {
        self.size
    }

    fn locate(&self, files: &[HostFile], offset: u64) -> Result<(Piece, u64), ReadError> {
        let cluster = self.cluster();
        let within = offset % cluster;
        let run = (cluster - within).min(self.size - offset);
        let unallocated = if self.backed { Piece::Parent } else { Piece::Zero };
        let index = offset >> self.cluster_bits;
        let l2_bits = self.cluster_bits - 3;
        let l1_entry = usize::try_from(index >> l2_bits)
            .ok()
            .and_then(|i| self.l1.get(i).copied())
            .unwrap_or(0);
        let l2_at = l1_entry & OFFSET_MASK;
        if l2_at == 0 {
            return Ok((unallocated, run));
        }
        let table = match self.l2(files, l2_at) {
            Ok(t) => t,
            Err(ReadError::Media) => return Ok((Piece::Bad, run)),
            Err(e) => return Err(e),
        };
        let slot = usize::try_from(index & ((1 << l2_bits) - 1)).map_err(|_| ReadError::Media)?;
        let entry = table.get(slot).copied().unwrap_or(0);
        if entry & COMPRESSED != 0 {
            if self.zstd {
                return Ok((Piece::Bad, run));
            }
            return Ok((Piece::Chunk { chunk: entry, within }, run));
        }
        if entry & ZERO_FLAG != 0 {
            return Ok((Piece::Zero, run));
        }
        let host = entry & OFFSET_MASK;
        if host == 0 {
            return Ok((unallocated, run));
        }
        if host % cluster != 0 {
            return Ok((Piece::Bad, run));
        }
        Ok((Piece::Data { file: 0, offset: host + within }, run))
    }

    fn decode(&self, files: &[HostFile], chunk: u64) -> Result<Vec<u8>, ReadError> {
        let (at, len) = self.compressed_span(chunk);
        let file = files.first().ok_or(ReadError::Media)?;
        // The last compressed cluster may end before its rounded-up sector count.
        let len = len.min(file.len.saturating_sub(at));
        let mut raw = vec![0u8; usize::try_from(len).map_err(|_| ReadError::Media)?];
        file.read(at, &mut raw)?;
        let limit = usize::try_from(self.cluster()).map_err(|_| ReadError::Media)?;
        let mut out = inflate_raw(&raw, limit)?;
        out.resize(limit, 0);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::testutil::{file_in, read_all};
    use crate::container::{ContainerFormat, open_image};
    use crate::testing::pattern;

    const CB: u32 = 12; // 4 KiB clusters, 512 L2 entries per table

    /// Cluster contents for the test image.
    enum C {
        Data(Vec<u8>),
        Compressed(Vec<u8>),
        Zero,
        Unallocated,
    }

    /// Builds a v3 image: header in cluster 0, L1 in cluster 1, one L2 table in cluster 2,
    /// data from cluster 3 on.
    fn build(clusters: &[C], backing: Option<&str>) -> Vec<u8> {
        let cs = 1usize << CB;
        let size = (clusters.len() * cs) as u64;
        let mut img = vec![0u8; 3 * cs];
        img[0..4].copy_from_slice(MAGIC);
        img[4..8].copy_from_slice(&3u32.to_be_bytes());
        img[20..24].copy_from_slice(&CB.to_be_bytes());
        img[24..32].copy_from_slice(&size.to_be_bytes());
        img[36..40].copy_from_slice(&1u32.to_be_bytes());
        img[40..48].copy_from_slice(&(cs as u64).to_be_bytes());
        img[96..100].copy_from_slice(&4u32.to_be_bytes());
        img[100..104].copy_from_slice(&104u32.to_be_bytes());
        if let Some(b) = backing {
            img[8..16].copy_from_slice(&512u64.to_be_bytes());
            img[16..20].copy_from_slice(&u32::try_from(b.len()).unwrap().to_be_bytes());
            img[512..512 + b.len()].copy_from_slice(b.as_bytes());
        }
        let l2_at = 2 * cs as u64;
        img[cs..cs + 8].copy_from_slice(&(l2_at | (1 << 63)).to_be_bytes());
        for (i, c) in clusters.iter().enumerate() {
            let slot = 2 * cs + i * 8;
            let entry = match c {
                C::Data(d) => {
                    let at = img.len() as u64;
                    img.extend_from_slice(d);
                    at | (1 << 63)
                }
                C::Compressed(d) => {
                    let z = miniz_oxide::deflate::compress_to_vec(d, 6);
                    let at = img.len() as u64;
                    img.extend_from_slice(&z);
                    img.resize(img.len().next_multiple_of(512), 0);
                    let sectors = (z.len() as u64 + (at & 511)).div_ceil(512) - 1;
                    let x = 62 - (u64::from(CB) - 8);
                    COMPRESSED | (sectors << x) | at
                }
                C::Zero => ZERO_FLAG,
                C::Unallocated => 0,
            };
            img[slot..slot + 8].copy_from_slice(&entry.to_be_bytes());
        }
        img.resize(img.len().next_multiple_of(cs), 0);
        img
    }

    #[test]
    fn reads_plain_compressed_zero_and_unallocated_clusters() {
        let dir = tempfile::tempdir().unwrap();
        let a = pattern(4096);
        let b: Vec<u8> = pattern(4096).into_iter().rev().collect();
        let img = build(&[C::Data(a.clone()), C::Compressed(b.clone()), C::Zero, C::Unallocated], None);
        let p = file_in(dir.path(), "disk.qcow2", &img);
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert_eq!(info.format, ContainerFormat::Qcow2);
        assert!(info.notes.is_empty(), "{:?}", info.notes);
        let all = read_all(src.as_ref());
        assert_eq!(&all[..4096], &a[..]);
        assert_eq!(&all[4096..8192], &b[..]);
        assert_eq!(&all[8192..], &vec![0u8; 8192][..]);
    }

    #[test]
    fn unallocated_clusters_come_from_the_backing_file() {
        let dir = tempfile::tempdir().unwrap();
        file_in(dir.path(), "base.raw", &pattern(8192));
        let mine = vec![0x5Au8; 4096];
        let img = build(&[C::Data(mine.clone()), C::Unallocated, C::Unallocated], Some("base.raw"));
        let p = file_in(dir.path(), "top.qcow2", &img);
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert_eq!(info.files.len(), 2);
        let all = read_all(src.as_ref());
        assert_eq!(&all[..4096], &mine[..]);
        assert_eq!(&all[4096..8192], &pattern(8192)[4096..]);
        // Past the end of the 8 KiB backing file the 12 KiB child reads zeros.
        assert_eq!(&all[8192..], &[0u8; 4096][..]);
    }

    #[test]
    fn damaged_compressed_cluster_is_unreadable_but_neighbours_are_fine() {
        let dir = tempfile::tempdir().unwrap();
        let a = pattern(4096);
        let mut img = build(&[C::Data(a.clone()), C::Compressed(pattern(4096))], None);
        // Garble the compressed stream (it starts right after the plain cluster).
        let at = 4 * 4096;
        for b in &mut img[at..at + 64] {
            *b ^= 0x5A;
        }
        let p = file_in(dir.path(), "d.qcow2", &img);
        let (src, _) = open_image(&p, 512, None).unwrap();
        let mut ok = vec![0u8; 4096];
        src.read_at(0, &mut ok).unwrap();
        assert_eq!(ok, a);
        let mut bad = vec![0u8; 512];
        assert!(src.read_at(4096, &mut bad).is_err());
    }

    #[test]
    fn l2_table_outside_the_file_is_unreadable_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let mut img = build(&[C::Data(pattern(4096))], None);
        img[4096..4104].copy_from_slice(&(0x7FFF_0000u64 | (1 << 63)).to_be_bytes());
        let p = file_in(dir.path(), "l2.qcow2", &img);
        let (src, _) = open_image(&p, 512, None).unwrap();
        let mut buf = vec![0u8; 512];
        assert!(matches!(src.read_at(0, &mut buf), Err(ReadError::Media)));
    }

    #[test]
    fn refuses_encryption_and_unknown_features() {
        let dir = tempfile::tempdir().unwrap();
        let mut img = build(&[C::Zero], None);
        img[32..36].copy_from_slice(&1u32.to_be_bytes());
        let p = file_in(dir.path(), "enc.qcow2", &img);
        assert!(matches!(open_image(&p, 512, None), Err(OpenError::Unsupported { .. })));
        let mut img = build(&[C::Zero], None);
        img[72..80].copy_from_slice(&(1u64 << 40).to_be_bytes());
        let p = file_in(dir.path(), "feat.qcow2", &img);
        assert!(matches!(open_image(&p, 512, None), Err(OpenError::Unsupported { .. })));
    }
}
