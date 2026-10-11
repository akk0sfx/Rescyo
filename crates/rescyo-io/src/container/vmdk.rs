//! VMDK (VMware Virtual Disk Format 1.1). Fields are little-endian.
//!
//! An image is either a sparse extent file (`KDMV` header, possibly with an embedded text
//! descriptor) or a text descriptor naming extents: `FLAT` (a raw file), `SPARSE` (a `KDMV`
//! file), `ZERO`, and the ESX `VMFS` flat kind. Sparse extents map grains through a grain
//! directory and grain tables; stream-optimized ones compress every grain with zlib and keep
//! the grain directory at the end of the file. A delta disk reads unallocated grains from the
//! parent named by `parentFileNameHint`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::{
    ContainerFormat, HostFile, Layout, OpenError, Opened, ParentRef, Piece, dir_of, foreign_path_candidates,
    inflate_zlib,
};
use crate::ReadError;
use crate::bytes::{slice_at, u16_at, u32_at, u64_at};

const F: ContainerFormat = ContainerFormat::Vmdk;
const SECTOR: u64 = 512;
const MAGIC: &[u8] = b"KDMV";
const GD_AT_END: u64 = u64::MAX;
const FLAG_REDUNDANT_GT: u32 = 1 << 1;
const FLAG_COMPRESSED: u32 = 1 << 16;
/// Largest descriptor read (they are a few hundred bytes).
const MAX_DESCRIPTOR: u64 = 64 * 1024;
/// Largest number of extents accepted from a descriptor.
const MAX_EXTENTS: usize = 4096;

/// Whether `head` is a VMDK text descriptor.
pub(super) fn looks_like_descriptor(head: &[u8]) -> bool {
    head.starts_with(b"# Disk DescriptorFile")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtentLine {
    sectors: u64,
    kind: String,
    file: Option<String>,
    offset: u64,
}

#[derive(Debug, Default)]
struct Descriptor {
    create_type: String,
    parent_cid: Option<String>,
    parent_hint: Option<String>,
    extents: Vec<ExtentLine>,
}

fn parse_descriptor(text: &str) -> Descriptor {
    let mut d = Descriptor::default();
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(e) = parse_extent(line) {
            if d.extents.len() < MAX_EXTENTS {
                d.extents.push(e);
            }
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"').to_string();
            match k.trim() {
                "createType" => d.create_type = v,
                "parentCID" => d.parent_cid = Some(v),
                "parentFileNameHint" => d.parent_hint = Some(v),
                _ => {}
            }
        }
    }
    d
}

/// `RW 4192256 SPARSE "disk-s001.vmdk"`, `RW 2048 FLAT "disk-flat.vmdk" 0`, `RW 2048 ZERO`.
fn parse_extent(line: &str) -> Option<ExtentLine> {
    let mut words = line.splitn(4, ' ').filter(|w| !w.is_empty());
    let access = words.next()?;
    if !matches!(access, "RW" | "RDONLY" | "NOACCESS") {
        return None;
    }
    let sectors = words.next()?.parse().ok()?;
    let kind = words.next()?.to_string();
    let rest = words.next().unwrap_or("").trim();
    let (file, tail) = match rest.strip_prefix('"') {
        Some(r) => {
            let end = r.find('"')?;
            (
                Some(r.get(..end)?.to_string()),
                r.get(end + 1..).unwrap_or("").trim(),
            )
        }
        None => (None, rest),
    };
    let offset = tail
        .split_whitespace()
        .next()
        .and_then(|o| o.parse().ok())
        .unwrap_or(0);
    Some(ExtentLine {
        sectors,
        kind,
        file,
        offset,
    })
}

#[derive(Debug, Clone, Copy)]
struct SparseHeader {
    capacity: u64,
    grain: u64,
    descriptor_offset: u64,
    descriptor_size: u64,
    gtes_per_gt: u64,
    rgd_offset: u64,
    gd_offset: u64,
    flags: u32,
    compress: u16,
}

fn parse_header(b: &[u8]) -> Option<SparseHeader> {
    if slice_at(b, 0, 4)? != MAGIC {
        return None;
    }
    Some(SparseHeader {
        flags: u32_at(b, 8)?,
        capacity: u64_at(b, 12)?,
        grain: u64_at(b, 20)?,
        descriptor_offset: u64_at(b, 28)?,
        descriptor_size: u64_at(b, 36)?,
        gtes_per_gt: u64::from(u32_at(b, 44)?),
        rgd_offset: u64_at(b, 48)?,
        gd_offset: u64_at(b, 56)?,
        compress: u16_at(b, 77)?,
    })
}

pub(super) fn open(path: &Path) -> Result<Opened, OpenError> {
    let dir = dir_of(path);
    let first = HostFile::open(path)?;
    let head = first.read_vec(0, usize::try_from(first.len.min(512)).unwrap_or(512))?;
    let mut notes = Vec::new();
    let (desc, embedded) = match parse_header(&head) {
        Some(h) => {
            let text = if h.descriptor_offset != 0 && h.descriptor_size != 0 {
                let len = (h.descriptor_size * SECTOR).min(MAX_DESCRIPTOR);
                first
                    .read_vec(h.descriptor_offset * SECTOR, usize::try_from(len).unwrap_or(0))
                    .ok()
                    .map(|b| String::from_utf8_lossy(&b).trim_end_matches('\0').to_string())
            } else {
                None
            };
            (text.map(|t| parse_descriptor(&t)), true)
        }
        None => {
            let len = first.len.min(MAX_DESCRIPTOR);
            let b = first.read_vec(0, usize::try_from(len).unwrap_or(0))?;
            (Some(parse_descriptor(&String::from_utf8_lossy(&b))), false)
        }
    };
    let mut files = vec![first];
    let mut extents = Vec::new();
    let mut start = 0u64;

    let lines = match &desc {
        Some(d) if !d.extents.is_empty() => d.extents.clone(),
        _ if embedded => {
            // A sparse file without a usable descriptor: one extent covering its capacity.
            notes.push("embedded descriptor is missing or unreadable; using the sparse header alone".into());
            vec![ExtentLine {
                sectors: 0,
                kind: "SPARSE".into(),
                file: None,
                offset: 0,
            }]
        }
        _ => return Err(OpenError::invalid(F, "descriptor lists no extents")),
    };
    for line in lines {
        let kind = line.kind.to_ascii_uppercase();
        let file_index = |files: &mut Vec<HostFile>| -> Result<usize, OpenError> {
            let Some(name) = &line.file else {
                return Ok(0);
            };
            let p = dir.join(name.replace('\\', "/"));
            if embedded && p.file_name() == path.file_name() {
                return Ok(0);
            }
            if let Some(i) = files.iter().position(|f| f.path == p) {
                return Ok(i);
            }
            files.push(HostFile::open(&p).map_err(|e| {
                OpenError::invalid(F, format!("extent {} cannot be opened: {e}", p.display()))
            })?);
            Ok(files.len() - 1)
        };
        let kind_piece = match kind.as_str() {
            "FLAT" | "VMFS" | "VMFSRAW" | "VMFSRDM" => {
                let file = file_index(&mut files)?;
                ExtentKind::Flat {
                    file,
                    offset: line.offset * SECTOR,
                }
            }
            "ZERO" => ExtentKind::Zero,
            "SPARSE" => {
                let file = file_index(&mut files)?;
                let host = files
                    .get(file)
                    .ok_or_else(|| OpenError::invalid(F, "extent file"))?;
                ExtentKind::Sparse(Box::new(open_sparse(host, file, &mut notes)?))
            }
            other => {
                return Err(OpenError::unsupported(F, format!("{other} extents")));
            }
        };
        let sectors = match (&kind_piece, line.sectors) {
            (ExtentKind::Sparse(s), 0) => s.header.capacity,
            (_, n) => n,
        };
        let bytes = sectors
            .checked_mul(SECTOR)
            .ok_or_else(|| OpenError::invalid(F, "extent size overflows"))?;
        extents.push(Extent {
            start,
            len: bytes,
            kind: kind_piece,
        });
        start = start
            .checked_add(bytes)
            .ok_or_else(|| OpenError::invalid(F, "disk size overflows"))?;
    }

    let parent = desc.as_ref().and_then(|d| {
        let cid = d.parent_cid.as_deref().unwrap_or("ffffffff");
        if cid.eq_ignore_ascii_case("ffffffff") {
            return None;
        }
        d.parent_hint.as_ref().map(|hint| ParentRef {
            candidates: foreign_path_candidates(&dir, hint),
            check: None,
        })
    });
    let backed = parent.is_some();
    Ok(Opened {
        files,
        layout: Box::new(VmdkLayout {
            size: start,
            extents,
            backed,
        }),
        logical_sector: 512,
        parent,
        notes,
    })
}

fn open_sparse(host: &HostFile, file: usize, notes: &mut Vec<String>) -> Result<Sparse, OpenError> {
    let raw = host.read_vec(0, 512)?;
    let mut header = parse_header(&raw)
        .ok_or_else(|| OpenError::invalid(F, format!("{} has no sparse header", host.path.display())))?;
    if header.gd_offset == GD_AT_END {
        // Stream-optimized: the real header is in the footer, 1024 bytes before the end.
        let at = host
            .len
            .checked_sub(1024)
            .ok_or_else(|| OpenError::invalid(F, "stream-optimized file has no footer"))?;
        let footer = host.read_vec(at, 512)?;
        header = parse_header(&footer)
            .filter(|h| h.gd_offset != GD_AT_END)
            .ok_or_else(|| OpenError::invalid(F, "stream-optimized footer is missing or damaged"))?;
    }
    if !header.grain.is_power_of_two() || header.grain < 8 || header.grain > 2048 {
        return Err(OpenError::invalid(
            F,
            format!("grain size {} sectors", header.grain),
        ));
    }
    if header.gtes_per_gt == 0 || header.gtes_per_gt > 1 << 16 {
        return Err(OpenError::invalid(
            F,
            format!("{} entries per grain table", header.gtes_per_gt),
        ));
    }
    if header.flags & FLAG_COMPRESSED != 0 && header.compress != 1 {
        return Err(OpenError::unsupported(
            F,
            format!("compression algorithm {}", header.compress),
        ));
    }
    let per_gd = header.grain * header.gtes_per_gt;
    let gd_entries = header.capacity.div_ceil(per_gd).min(host.len / 4);
    let read_gd = |sector: u64| -> Option<Vec<u32>> {
        if sector == 0 {
            return None;
        }
        let b = host
            .read_vec(sector.checked_mul(SECTOR)?, usize::try_from(gd_entries * 4).ok()?)
            .ok()?;
        Some(
            b.as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect(),
        )
    };
    let gd = read_gd(header.gd_offset);
    let rgd = if header.flags & FLAG_REDUNDANT_GT != 0 {
        read_gd(header.rgd_offset)
    } else {
        None
    };
    let (gd, rgd) = match (gd, rgd) {
        (Some(g), r) => (g, r),
        (None, Some(r)) => {
            notes.push(format!(
                "{}: grain directory is damaged; using the redundant copy",
                host.path.display()
            ));
            (r, None)
        }
        (None, None) => return Err(OpenError::invalid(F, "grain directory is outside the file")),
    };
    Ok(Sparse {
        file,
        header,
        gd,
        rgd,
        gt_cache: Mutex::new(Vec::new()),
    })
}

#[derive(Debug)]
struct Sparse {
    file: usize,
    header: SparseHeader,
    gd: Vec<u32>,
    rgd: Option<Vec<u32>>,
    gt_cache: Mutex<Vec<(u64, Arc<Vec<u32>>)>>,
}

impl Sparse {
    fn gt(&self, files: &[HostFile], sector: u64) -> Result<Arc<Vec<u32>>, ReadError> {
        if let Ok(c) = self.gt_cache.lock()
            && let Some((_, t)) = c.iter().find(|(s, _)| *s == sector)
        {
            return Ok(Arc::clone(t));
        }
        let mut raw = vec![0u8; usize::try_from(self.header.gtes_per_gt * 4).map_err(|_| ReadError::Media)?];
        let at = sector.checked_mul(SECTOR).ok_or(ReadError::Media)?;
        files.get(self.file).ok_or(ReadError::Media)?.read(at, &mut raw)?;
        let t = Arc::new(
            raw.as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect::<Vec<_>>(),
        );
        if let Ok(mut c) = self.gt_cache.lock() {
            if c.len() >= 32 {
                c.remove(0);
            }
            c.push((sector, Arc::clone(&t)));
        }
        Ok(t)
    }

    /// The grain entry for grain `g`, trying the redundant directory when the primary fails.
    fn grain_entry(&self, files: &[HostFile], g: u64) -> Result<Option<u32>, ReadError> {
        let dir_index = usize::try_from(g / self.header.gtes_per_gt).map_err(|_| ReadError::Media)?;
        let slot = usize::try_from(g % self.header.gtes_per_gt).map_err(|_| ReadError::Media)?;
        let mut last = Ok(None);
        for gd in std::iter::once(&self.gd).chain(self.rgd.as_ref()) {
            let gt_sector = u64::from(gd.get(dir_index).copied().unwrap_or(0));
            if gt_sector == 0 {
                return Ok(None);
            }
            match self.gt(files, gt_sector) {
                Ok(t) => return Ok(t.get(slot).copied()),
                Err(e) => last = Err(e),
            }
        }
        last
    }
}

#[derive(Debug)]
enum ExtentKind {
    Flat { file: usize, offset: u64 },
    Zero,
    Sparse(Box<Sparse>),
}

#[derive(Debug)]
struct Extent {
    start: u64,
    len: u64,
    kind: ExtentKind,
}

#[derive(Debug)]
struct VmdkLayout {
    size: u64,
    extents: Vec<Extent>,
    backed: bool,
}

/// Compressed grain ids: extent index in the top 16 bits, grain sector below.
fn chunk_id(extent: usize, sector: u64) -> u64 {
    ((extent as u64) << 48) | (sector & ((1 << 48) - 1))
}

impl Layout for VmdkLayout {
    fn size(&self) -> u64 {
        self.size
    }

    fn locate(&self, files: &[HostFile], offset: u64) -> Result<(Piece, u64), ReadError> {
        let i = self.extents.partition_point(|e| e.start + e.len <= offset);
        let e = self.extents.get(i).ok_or(ReadError::OutOfBounds)?;
        let within = offset - e.start;
        let left = e.len - within;
        match &e.kind {
            ExtentKind::Zero => Ok((Piece::Zero, left)),
            ExtentKind::Flat { file, offset: base } => Ok((
                Piece::Data {
                    file: *file,
                    offset: base + within,
                },
                left,
            )),
            ExtentKind::Sparse(s) => {
                let grain = s.header.grain * SECTOR;
                let g = within / grain;
                let in_grain = within % grain;
                let run = (grain - in_grain).min(left);
                let entry = match s.grain_entry(files, g) {
                    Ok(v) => v,
                    Err(ReadError::Media) => return Ok((Piece::Bad, run)),
                    Err(e) => return Err(e),
                };
                let unallocated = if self.backed { Piece::Parent } else { Piece::Zero };
                match entry {
                    None | Some(0) => Ok((unallocated, run)),
                    Some(1) => Ok((Piece::Zero, run)),
                    Some(sector) if s.header.flags & FLAG_COMPRESSED != 0 => Ok((
                        Piece::Chunk {
                            chunk: chunk_id(i, u64::from(sector)),
                            within: in_grain,
                        },
                        run,
                    )),
                    Some(sector) => Ok((
                        Piece::Data {
                            file: s.file,
                            offset: u64::from(sector) * SECTOR + in_grain,
                        },
                        run,
                    )),
                }
            }
        }
    }

    fn decode(&self, files: &[HostFile], chunk: u64) -> Result<Vec<u8>, ReadError> {
        let i = usize::try_from(chunk >> 48).map_err(|_| ReadError::Media)?;
        let sector = chunk & ((1 << 48) - 1);
        let Some(ExtentKind::Sparse(s)) = self.extents.get(i).map(|e| &e.kind) else {
            return Err(ReadError::Media);
        };
        let file = files.get(s.file).ok_or(ReadError::Media)?;
        let at = sector.checked_mul(SECTOR).ok_or(ReadError::Media)?;
        // Grain marker: LBA (u64), compressed size (u32), then the zlib stream.
        let mut head = [0u8; 12];
        file.read(at, &mut head)?;
        let size = u64::from(u32_at(&head, 8).ok_or(ReadError::Media)?);
        let grain = s.header.grain * SECTOR;
        if size == 0 || size > grain + 4096 {
            return Err(ReadError::Media);
        }
        let mut data = vec![0u8; usize::try_from(size).map_err(|_| ReadError::Media)?];
        file.read(at + 12, &mut data)?;
        let limit = usize::try_from(grain).map_err(|_| ReadError::Media)?;
        let mut out = inflate_zlib(&data, limit)?;
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

    const GRAIN: u64 = 8; // sectors: 4 KiB grains
    const GTES: u32 = 512;

    fn header(capacity: u64, gd: u64, rgd: u64, flags: u32, desc: (u64, u64)) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[0..4].copy_from_slice(MAGIC);
        h[4..8].copy_from_slice(&1u32.to_le_bytes());
        h[8..12].copy_from_slice(&(flags | 1).to_le_bytes());
        h[12..20].copy_from_slice(&capacity.to_le_bytes());
        h[20..28].copy_from_slice(&GRAIN.to_le_bytes());
        h[28..36].copy_from_slice(&desc.0.to_le_bytes());
        h[36..44].copy_from_slice(&desc.1.to_le_bytes());
        h[44..48].copy_from_slice(&GTES.to_le_bytes());
        h[48..56].copy_from_slice(&rgd.to_le_bytes());
        h[56..64].copy_from_slice(&gd.to_le_bytes());
        h[73..77].copy_from_slice(b"\n \r\n");
        if flags & FLAG_COMPRESSED != 0 {
            h[77..79].copy_from_slice(&1u16.to_le_bytes());
        }
        h
    }

    /// Monolithic sparse file: header (sector 0), descriptor (sectors 1–2), redundant GD
    /// (sector 3), redundant GT (sectors 4–7), GD (sector 8), GT (sectors 9–12), grains from
    /// sector 16.
    fn sparse_file(grains: &[Option<Vec<u8>>], descriptor: &str, compressed: bool) -> Vec<u8> {
        let capacity = grains.len() as u64 * GRAIN;
        let flags = FLAG_REDUNDANT_GT
            | if compressed {
                FLAG_COMPRESSED | (1 << 17)
            } else {
                0
            };
        let mut img = header(capacity, 8, 3, flags, (1, 2));
        img.resize(16 * 512, 0);
        img[512..512 + descriptor.len()].copy_from_slice(descriptor.as_bytes());
        for gd in [3usize, 8] {
            let gt = if gd == 3 { 4u32 } else { 9u32 };
            img[gd * 512..gd * 512 + 4].copy_from_slice(&gt.to_le_bytes());
        }
        for (i, g) in grains.iter().enumerate() {
            let Some(g) = g else { continue };
            let sector = u32::try_from(img.len() / 512).unwrap();
            if compressed {
                let z = miniz_oxide::deflate::compress_to_vec_zlib(g, 6);
                img.extend_from_slice(&(i as u64 * GRAIN).to_le_bytes());
                img.extend_from_slice(&u32::try_from(z.len()).unwrap().to_le_bytes());
                img.extend_from_slice(&z);
                img.resize(img.len().next_multiple_of(512), 0);
            } else {
                img.extend_from_slice(g);
            }
            for gt in [4usize, 9] {
                img[gt * 512 + i * 4..gt * 512 + i * 4 + 4].copy_from_slice(&sector.to_le_bytes());
            }
        }
        img
    }

    #[test]
    fn monolithic_sparse_reads_grains_and_zero_fills_holes() {
        let dir = tempfile::tempdir().unwrap();
        let desc = "# Disk DescriptorFile\nversion=1\nparentCID=ffffffff\ncreateType=\"monolithicSparse\"\nRW 24 SPARSE \"disk.vmdk\"\n";
        let img = sparse_file(&[Some(pattern(4096)), None, Some(vec![7; 4096])], desc, false);
        let p = file_in(dir.path(), "disk.vmdk", &img);
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert_eq!(info.format, ContainerFormat::Vmdk);
        assert!(info.notes.is_empty(), "{:?}", info.notes);
        let all = read_all(src.as_ref());
        assert_eq!(&all[..4096], &pattern(4096)[..]);
        assert_eq!(&all[4096..8192], &[0u8; 4096][..]);
        assert_eq!(&all[8192..], &[7u8; 4096][..]);
    }

    #[test]
    fn compressed_grains_decode() {
        let dir = tempfile::tempdir().unwrap();
        let desc = "# Disk DescriptorFile\ncreateType=\"streamOptimized\"\nRW 16 SPARSE \"s.vmdk\"\n";
        let img = sparse_file(&[None, Some(pattern(4096))], desc, true);
        let p = file_in(dir.path(), "s.vmdk", &img);
        let (src, _) = open_image(&p, 512, None).unwrap();
        let all = read_all(src.as_ref());
        assert_eq!(&all[..4096], &[0u8; 4096][..]);
        assert_eq!(&all[4096..], &pattern(4096)[..]);
    }

    #[test]
    fn damaged_grain_directory_falls_back_to_the_redundant_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut img = sparse_file(&[Some(pattern(4096))], "", false);
        // Point the primary grain table far outside the file.
        img[8 * 512..8 * 512 + 4].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes());
        let p = file_in(dir.path(), "r.vmdk", &img);
        let (src, _) = open_image(&p, 512, None).unwrap();
        assert_eq!(read_all(src.as_ref()), pattern(4096));
    }

    #[test]
    fn descriptor_with_flat_and_zero_extents() {
        let dir = tempfile::tempdir().unwrap();
        file_in(dir.path(), "d-flat.vmdk", &pattern(8192));
        let desc = "# Disk DescriptorFile\nversion=1\nCID=1\nparentCID=ffffffff\ncreateType=\"monolithicFlat\"\n\n# Extent description\nRW 8 FLAT \"d-flat.vmdk\" 8\nRW 8 ZERO\n";
        let p = file_in(dir.path(), "d.vmdk", desc.as_bytes());
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert_eq!(info.files.len(), 2);
        let all = read_all(src.as_ref());
        assert_eq!(&all[..4096], &pattern(8192)[4096..]);
        assert_eq!(&all[4096..], &[0u8; 4096][..]);
    }

    #[test]
    fn delta_disk_reads_unallocated_grains_from_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        file_in(dir.path(), "base.img", &pattern(8192));
        let desc = "# Disk DescriptorFile\nparentCID=1234abcd\ncreateType=\"monolithicSparse\"\nparentFileNameHint=\"base.img\"\nRW 16 SPARSE \"delta.vmdk\"\n";
        let img = sparse_file(&[None, Some(vec![9; 4096])], desc, false);
        let p = file_in(dir.path(), "delta.vmdk", &img);
        let (src, _) = open_image(&p, 512, None).unwrap();
        let all = read_all(src.as_ref());
        assert_eq!(&all[..4096], &pattern(8192)[..4096]);
        assert_eq!(&all[4096..], &[9u8; 4096][..]);
    }

    #[test]
    fn extent_lines_parse() {
        assert_eq!(
            parse_extent("RW 2048 FLAT \"my disk-flat.vmdk\" 128"),
            Some(ExtentLine {
                sectors: 2048,
                kind: "FLAT".into(),
                file: Some("my disk-flat.vmdk".into()),
                offset: 128
            })
        );
        assert_eq!(parse_extent("ddb.virtualHWVersion = \"4\""), None);
        assert_eq!(parse_extent("RW lots SPARSE \"x\""), None);
    }
}
