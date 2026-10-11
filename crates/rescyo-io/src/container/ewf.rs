//! Expert Witness Compression Format (EWF): EnCase `.E01` and SMART `.S01` evidence files,
//! following the public format documentation of the libewf project. Fields are little-endian.
//!
//! An image is split into numbered segment files (`.E01`, `.E02`, … `.E99`, `.EAA`, …). Each
//! holds a chain of sections; `volume`/`disk` gives the geometry, `sectors` holds the chunks
//! (zlib-compressed, or stored with an Adler-32), and `table` (mirrored by `table2`) lists
//! where each chunk starts. A chunk that fails its checksum or does not decompress is
//! unreadable; the rest of the image is not affected.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{ContainerFormat, HostFile, Layout, OpenError, Opened, Piece, inflate_zlib};
use crate::ReadError;
use crate::bytes::{slice_at, u32_at, u64_at};

const F: ContainerFormat = ContainerFormat::Ewf;

/// Segment-file signature of EnCase (E01) evidence files.
pub(super) const SIGNATURE: &[u8] = b"EVF\x09\x0d\x0a\xff\x00";
/// Segment-file signature of SMART (S01) evidence files.
pub(super) const SIGNATURE_SMART: &[u8] = b"LVF\x09\x0d\x0a\xff\x00";

const FILE_HEADER: u64 = 13;
const DESCRIPTOR: u64 = 76;
/// Bound on the sections walked in one segment file.
const MAX_SECTIONS: usize = 1 << 20;
/// Bound on the number of segment files (E01…ZZZ is 14 971 names).
const MAX_SEGMENTS: usize = 15_000;
/// EnCase never writes more entries than this into one table.
const MAX_TABLE_ENTRIES: u64 = 1 << 20;

pub(crate) fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

#[derive(Debug, Clone)]
struct Section {
    kind: String,
    start: u64,
    next: u64,
    size: u64,
}

fn read_section(file: &HostFile, at: u64) -> Option<Section> {
    let d = file.read_vec(at, usize::try_from(DESCRIPTOR).ok()?).ok()?;
    let kind = slice_at(&d, 0, 16)?;
    let end = kind.iter().position(|&b| b == 0).unwrap_or(16);
    let kind = std::str::from_utf8(kind.get(..end)?).ok()?.to_ascii_lowercase();
    if kind.is_empty() || !kind.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(Section {
        kind,
        start: at,
        next: u64_at(&d, 16)?,
        size: u64_at(&d, 24)?,
    })
}

/// The sections of one segment file, in chain order.
fn walk(file: &HostFile) -> Vec<Section> {
    let mut out = Vec::new();
    let mut at = FILE_HEADER;
    while out.len() < MAX_SECTIONS {
        let Some(s) = read_section(file, at) else {
            break;
        };
        let next = s.next;
        let last = matches!(s.kind.as_str(), "done" | "next") || next <= at || next >= file.len;
        out.push(s);
        if last {
            break;
        }
        at = next;
    }
    out
}

/// Names of the segment after `name`: `E01`→`E02`, `E99`→`EAA`, `EZZ`→`FAA` (case kept).
fn next_extension(ext: &str) -> Option<String> {
    let b = ext.as_bytes();
    let (&c0, &c1, &c2) = (b.first()?, b.get(1)?, b.get(2)?);
    if b.len() != 3 {
        return None;
    }
    let upper = c0.is_ascii_uppercase();
    let base = if upper { b'A' } else { b'a' };
    let next = if c1.is_ascii_digit() && c2.is_ascii_digit() {
        let n = u32::from(c1 - b'0') * 10 + u32::from(c2 - b'0');
        if n < 99 {
            return Some(format!("{}{:02}", char::from(c0), n + 1));
        }
        [c0, base, base]
    } else {
        let (l0, l1, l2) = (
            c0.to_ascii_lowercase(),
            c1.to_ascii_lowercase(),
            c2.to_ascii_lowercase(),
        );
        let mut v = [l0 - b'a', l1 - b'a', l2 - b'a'];
        if v.iter().any(|&x| x > 25) {
            return None;
        }
        v[2] += 1;
        if v[2] == 26 {
            v[2] = 0;
            v[1] += 1;
        }
        if v[1] == 26 {
            v[1] = 0;
            v[0] += 1;
        }
        if v[0] == 26 {
            return None;
        }
        [base + v[0], base + v[1], base + v[2]]
    };
    String::from_utf8(next.to_vec()).ok()
}

/// The first segment of the set `path` belongs to (`x.E03` → `x.E01`).
fn first_segment(path: &Path) -> PathBuf {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return path.to_path_buf();
    };
    let b = ext.as_bytes();
    if b.len() == 3 && b.first().is_some_and(u8::is_ascii_alphabetic) {
        let first = format!("{}01", ext.get(..1).unwrap_or("E"));
        let candidate = path.with_extension(first);
        if candidate.is_file() {
            return candidate;
        }
    }
    path.to_path_buf()
}

#[derive(Debug, Clone, Copy)]
struct Volume {
    chunks: u64,
    sectors_per_chunk: u64,
    bytes_per_sector: u64,
    sectors: u64,
}

fn parse_volume(data: &[u8]) -> Option<Volume> {
    let chunks = u64::from(u32_at(data, 4)?);
    let sectors_per_chunk = u64::from(u32_at(data, 8)?);
    let bytes_per_sector = u64::from(u32_at(data, 12)?);
    // EnCase writes a 64-bit sector count in a 1052-byte volume section; SMART a 32-bit one.
    let sectors = if data.len() >= 1052 {
        u64_at(data, 16)?
    } else {
        u64::from(u32_at(data, 16)?)
    };
    Some(Volume {
        chunks,
        sectors_per_chunk,
        bytes_per_sector,
        sectors,
    })
}

/// One `table` (or `table2`) section: the start of each chunk of a run of chunks.
#[derive(Debug, Clone)]
struct Table {
    file: usize,
    /// Offset of the entry array.
    entries_at: u64,
    count: u64,
    base: u64,
    /// The mirror `table2` entry array, if present.
    mirror_at: Option<u64>,
    /// First global chunk number described by this table.
    first_chunk: u64,
}

fn table_header(file: &HostFile, s: &Section) -> Option<(u64, u64, bool)> {
    let h = file.read_vec(s.start + DESCRIPTOR, 24).ok()?;
    let count = u64::from(u32_at(&h, 0)?);
    let base = u64_at(&h, 8)?;
    let ok = u32_at(&h, 20)? == adler32(slice_at(&h, 0, 20)?);
    Some((count, base, ok))
}

pub(super) fn open(path: &Path) -> Result<Opened, OpenError> {
    let first = first_segment(path);
    let mut files = Vec::new();
    let mut notes = Vec::new();
    let mut volume = None;
    let mut tables: Vec<Table> = Vec::new();
    let mut section_starts: Vec<Vec<u64>> = Vec::new();
    let mut next_chunk = 0u64;
    let mut seg_path = first.clone();
    let mut done = false;
    while files.len() < MAX_SEGMENTS {
        let Ok(file) = HostFile::open(&seg_path) else {
            break;
        };
        let head = file.read_vec(0, 13)?;
        if !(head.starts_with(SIGNATURE) || head.starts_with(SIGNATURE_SMART)) {
            if files.is_empty() {
                return Err(OpenError::invalid(F, "not an EWF segment file"));
            }
            break;
        }
        let sections = walk(&file);
        let index = files.len();
        let mut starts: Vec<u64> = sections.iter().map(|s| s.start).collect();
        starts.push(file.len);
        starts.sort_unstable();
        section_starts.push(starts);
        let mut i = 0usize;
        while let Some(s) = sections.get(i) {
            match s.kind.as_str() {
                "volume" | "disk" if volume.is_none() => {
                    let len = s.size.saturating_sub(DESCRIPTOR).min(1052);
                    if let Ok(d) = file.read_vec(s.start + DESCRIPTOR, usize::try_from(len).unwrap_or(0)) {
                        volume = parse_volume(&d);
                    }
                }
                "table" => {
                    let mirror = sections.get(i + 1).filter(|m| m.kind == "table2");
                    let primary = table_header(&file, s);
                    let mirror_header = mirror.and_then(|m| table_header(&file, m));
                    let (count, base) = match (primary, mirror_header) {
                        (Some((c, b, true)), _) => (c, b),
                        (_, Some((c, b, true))) => {
                            notes.push(format!(
                                "{}: damaged table header; used its mirror",
                                seg_path.display()
                            ));
                            (c, b)
                        }
                        (Some((c, b, false)), _) => {
                            notes.push(format!(
                                "{}: table header checksum does not match",
                                seg_path.display()
                            ));
                            (c, b)
                        }
                        _ => {
                            notes.push(format!("{}: unreadable chunk table skipped", seg_path.display()));
                            i += 1;
                            continue;
                        }
                    };
                    let count = count.min(MAX_TABLE_ENTRIES);
                    tables.push(Table {
                        file: index,
                        entries_at: s.start + DESCRIPTOR + 24,
                        count,
                        base,
                        mirror_at: mirror.map(|m| m.start + DESCRIPTOR + 24),
                        first_chunk: next_chunk,
                    });
                    next_chunk += count;
                    if mirror.is_some() {
                        i += 1;
                    }
                }
                "done" => done = true,
                _ => {}
            }
            i += 1;
        }
        files.push(file);
        if done {
            break;
        }
        let Some(ext) = seg_path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(next_extension)
        else {
            break;
        };
        seg_path = seg_path.with_extension(ext);
    }
    let volume = volume.ok_or_else(|| OpenError::invalid(F, "no volume section found"))?;
    if !done {
        notes.push(format!(
            "the set ends after {} segment file(s) without a closing section; later segments may be missing",
            files.len()
        ));
    }
    let chunk_size = volume
        .sectors_per_chunk
        .checked_mul(volume.bytes_per_sector)
        .filter(|c| (512..=16 << 20).contains(c))
        .ok_or_else(|| OpenError::invalid(F, "implausible chunk size"))?;
    if volume.bytes_per_sector != 512 && volume.bytes_per_sector != 4096 {
        return Err(OpenError::unsupported(
            F,
            format!("{}-byte sectors", volume.bytes_per_sector),
        ));
    }
    let size = volume
        .sectors
        .checked_mul(volume.bytes_per_sector)
        .ok_or_else(|| OpenError::invalid(F, "size overflows"))?;
    if next_chunk < volume.chunks {
        notes.push(format!(
            "tables describe {next_chunk} of {} chunks; the rest is unreadable",
            volume.chunks
        ));
    }
    let logical_sector = u32::try_from(volume.bytes_per_sector).unwrap_or(512);
    Ok(Opened {
        files,
        layout: Box::new(EwfLayout {
            size,
            chunk_size,
            tables,
            section_starts,
            cache: Mutex::new(Vec::new()),
        }),
        logical_sector,
        parent: None,
        notes,
    })
}

#[derive(Debug)]
struct EwfLayout {
    size: u64,
    chunk_size: u64,
    tables: Vec<Table>,
    /// Sorted section starts (and the file length) of each segment file: the last chunk of a
    /// table ends where the next section begins.
    section_starts: Vec<Vec<u64>>,
    cache: Mutex<Vec<(usize, Arc<Vec<u32>>)>>,
}

impl EwfLayout {
    fn read_entries(file: &HostFile, at: u64, count: u64) -> Result<Vec<u32>, ReadError> {
        let len = usize::try_from(count * 4).map_err(|_| ReadError::Media)?;
        let mut raw = vec![0u8; len];
        file.read(at, &mut raw)?;
        // EnCase 6+ follows the entries with their Adler-32; older versions do not.
        let mut sum = [0u8; 4];
        if file.read(at + count * 4, &mut sum).is_ok() && u32::from_le_bytes(sum) != adler32(&raw) {
            // A mismatch here is not proof of damage (older writers), but it is a reason to
            // prefer the mirror: the caller tries it first.
            return Err(ReadError::Media);
        }
        Ok(raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect())
    }

    fn entries(&self, files: &[HostFile], t: usize) -> Result<Arc<Vec<u32>>, ReadError> {
        if let Ok(c) = self.cache.lock()
            && let Some((_, e)) = c.iter().find(|(i, _)| *i == t)
        {
            return Ok(Arc::clone(e));
        }
        let table = self.tables.get(t).ok_or(ReadError::Media)?;
        let file = files.get(table.file).ok_or(ReadError::Media)?;
        let strict =
            Self::read_entries(file, table.entries_at, table.count).or_else(|e| match table.mirror_at {
                Some(m) => Self::read_entries(file, m, table.count),
                None => Err(e),
            });
        let entries = match strict {
            Ok(e) => e,
            // Neither copy has a matching checksum: use the primary as it is.
            Err(_) => {
                let len = usize::try_from(table.count * 4).map_err(|_| ReadError::Media)?;
                let mut raw = vec![0u8; len];
                file.read(table.entries_at, &mut raw)?;
                raw.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| u32::from_le_bytes(*c))
                    .collect()
            }
        };
        let entries = Arc::new(entries);
        if let Ok(mut c) = self.cache.lock() {
            if c.len() >= 8 {
                c.remove(0);
            }
            c.push((t, Arc::clone(&entries)));
        }
        Ok(entries)
    }
}

impl Layout for EwfLayout {
    fn size(&self) -> u64 {
        self.size
    }

    fn locate(&self, _: &[HostFile], offset: u64) -> Result<(Piece, u64), ReadError> {
        let chunk = offset / self.chunk_size;
        let within = offset % self.chunk_size;
        let run = (self.chunk_size - within).min(self.size - offset);
        let described = self
            .tables
            .last()
            .is_some_and(|t| chunk < t.first_chunk.saturating_add(t.count));
        if !described {
            return Ok((Piece::Bad, run));
        }
        Ok((Piece::Chunk { chunk, within }, run))
    }

    fn decode(&self, files: &[HostFile], chunk: u64) -> Result<Vec<u8>, ReadError> {
        let t = self
            .tables
            .partition_point(|t| t.first_chunk.saturating_add(t.count) <= chunk);
        let table = self.tables.get(t).ok_or(ReadError::Media)?;
        let entries = self.entries(files, t)?;
        let i = usize::try_from(chunk - table.first_chunk).map_err(|_| ReadError::Media)?;
        let entry = *entries.get(i).ok_or(ReadError::Media)?;
        let compressed = entry & 0x8000_0000 != 0;
        let start = u64::from(entry & 0x7FFF_FFFF) + table.base;
        let end = match entries.get(i + 1) {
            Some(&n) => u64::from(n & 0x7FFF_FFFF) + table.base,
            None => self
                .section_starts
                .get(table.file)
                .and_then(|st| st.iter().copied().find(|&x| x > start))
                .ok_or(ReadError::Media)?,
        };
        let limit = usize::try_from(self.chunk_size).map_err(|_| ReadError::Media)?;
        let len = end
            .checked_sub(start)
            .filter(|&l| l > 0 && l <= self.chunk_size + 4096);
        let file = files.get(table.file).ok_or(ReadError::Media)?;
        let len = match len {
            Some(l) => l,
            // The size cannot be trusted: a stored chunk is chunk_size + 4 bytes.
            None if !compressed => self.chunk_size + 4,
            None => return Err(ReadError::Media),
        };
        let mut raw = vec![0u8; usize::try_from(len).map_err(|_| ReadError::Media)?];
        file.read(start, &mut raw)?;
        if compressed {
            let mut out = inflate_zlib(&raw, limit)?;
            out.resize(limit, 0);
            return Ok(out);
        }
        // A stored chunk: data, then its Adler-32. The last chunk of the image may be short.
        let data_len = raw.len().saturating_sub(4).min(limit);
        let (data, sum) = raw.split_at(data_len);
        let stored = u32_at(sum, 0).ok_or(ReadError::Media)?;
        if stored != adler32(data) {
            return Err(ReadError::Media);
        }
        let mut out = data.to_vec();
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

    const SPC: u32 = 8; // 4 KiB chunks

    fn section(kind: &str, start: u64, next: u64, data: &[u8]) -> Vec<u8> {
        let mut d = vec![0u8; 76];
        d[..kind.len()].copy_from_slice(kind.as_bytes());
        d[16..24].copy_from_slice(&next.to_le_bytes());
        d[24..32].copy_from_slice(&(76 + data.len() as u64).to_le_bytes());
        let c = adler32(&d[..72]);
        d[72..76].copy_from_slice(&c.to_le_bytes());
        let _ = start;
        d.extend_from_slice(data);
        d
    }

    fn volume(chunks: u32, sectors: u64) -> Vec<u8> {
        let mut v = vec![0u8; 1052];
        v[0] = 1;
        v[4..8].copy_from_slice(&chunks.to_le_bytes());
        v[8..12].copy_from_slice(&SPC.to_le_bytes());
        v[12..16].copy_from_slice(&512u32.to_le_bytes());
        v[16..24].copy_from_slice(&sectors.to_le_bytes());
        let c = adler32(&v[..1048]);
        v[1048..1052].copy_from_slice(&c.to_le_bytes());
        v
    }

    fn table(offsets: &[(u64, bool)], base: u64) -> Vec<u8> {
        let mut t = vec![0u8; 24];
        t[0..4].copy_from_slice(&u32::try_from(offsets.len()).unwrap().to_le_bytes());
        t[8..16].copy_from_slice(&base.to_le_bytes());
        let c = adler32(&t[..20]);
        t[20..24].copy_from_slice(&c.to_le_bytes());
        let mut entries = Vec::new();
        for &(o, comp) in offsets {
            let e = u32::try_from(o - base).unwrap() | if comp { 0x8000_0000 } else { 0 };
            entries.extend_from_slice(&e.to_le_bytes());
        }
        let c = adler32(&entries);
        t.extend_from_slice(&entries);
        t.extend_from_slice(&c.to_le_bytes());
        t
    }

    /// One segment with `chunks` (data, compress?) and, when `last`, a `done` section.
    fn segment(number: u16, chunks: &[(Vec<u8>, bool)], vol: Option<(u32, u64)>, last: bool) -> Vec<u8> {
        let mut img = SIGNATURE.to_vec();
        img.push(1);
        img.extend_from_slice(&number.to_le_bytes());
        img.extend_from_slice(&0u16.to_le_bytes());
        if let Some((c, s)) = vol {
            let data = volume(c, s);
            let next = img.len() as u64 + 76 + data.len() as u64;
            img.extend(section("volume", img.len() as u64, next, &data));
        }
        // sectors section
        let mut payload = Vec::new();
        let sectors_start = img.len() as u64;
        let mut offsets = Vec::new();
        for (c, comp) in chunks {
            offsets.push((sectors_start + 76 + payload.len() as u64, *comp));
            if *comp {
                payload.extend(miniz_oxide::deflate::compress_to_vec_zlib(c, 6));
            } else {
                payload.extend_from_slice(c);
                payload.extend_from_slice(&adler32(c).to_le_bytes());
            }
        }
        let next = sectors_start + 76 + payload.len() as u64;
        img.extend(section("sectors", sectors_start, next, &payload));
        let t = table(&offsets, 0);
        for kind in ["table", "table2"] {
            let next = img.len() as u64 + 76 + t.len() as u64;
            img.extend(section(kind, img.len() as u64, next, &t));
        }
        let at = img.len() as u64;
        img.extend(section(if last { "done" } else { "next" }, at, at, &[]));
        img
    }

    #[test]
    fn reads_compressed_and_stored_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let a = pattern(4096);
        let b = vec![3u8; 4096];
        let img = segment(1, &[(a.clone(), true), (b.clone(), false)], Some((2, 16)), true);
        let p = file_in(dir.path(), "case.E01", &img);
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert_eq!(info.format, ContainerFormat::Ewf);
        assert!(info.notes.is_empty(), "{:?}", info.notes);
        let all = read_all(src.as_ref());
        assert_eq!(&all[..4096], &a[..]);
        assert_eq!(&all[4096..], &b[..]);
    }

    #[test]
    fn follows_segments_and_opens_from_any_of_them() {
        let dir = tempfile::tempdir().unwrap();
        let s1 = segment(1, &[(pattern(4096), true)], Some((2, 16)), false);
        let s2 = segment(2, &[(vec![5u8; 4096], true)], None, true);
        file_in(dir.path(), "x.E01", &s1);
        let p2 = file_in(dir.path(), "x.E02", &s2);
        let (src, info) = open_image(&p2, 512, None).unwrap();
        assert_eq!(info.files.len(), 2, "{:?}", info.files);
        let all = read_all(src.as_ref());
        assert_eq!(&all[..4096], &pattern(4096)[..]);
        assert_eq!(&all[4096..], &[5u8; 4096][..]);
    }

    #[test]
    fn a_chunk_failing_its_checksum_is_unreadable_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut img = segment(
            1,
            &[(pattern(4096), false), (vec![1u8; 4096], false)],
            Some((2, 16)),
            true,
        );
        // Flip a byte inside the first stored chunk (after header, volume and the sectors descriptor).
        let at = 13 + 76 + 1052 + 76 + 100;
        img[at] ^= 0xFF;
        let p = file_in(dir.path(), "bad.E01", &img);
        let (src, _) = open_image(&p, 512, None).unwrap();
        let mut buf = vec![0u8; 512];
        assert!(matches!(src.read_at(0, &mut buf), Err(ReadError::Media)));
        let mut ok = vec![0u8; 4096];
        src.read_at(4096, &mut ok).unwrap();
        assert_eq!(ok, vec![1u8; 4096]);
    }

    #[test]
    fn damaged_table_uses_its_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let mut img = segment(
            1,
            &[(pattern(4096), true), (vec![2u8; 4096], true)],
            Some((2, 16)),
            true,
        );
        // The first table starts after header, volume and sectors; garble its entries.
        let sectors_len =
            u64::from_le_bytes(img[13 + 76 + 1052 + 24..13 + 76 + 1052 + 32].try_into().unwrap());
        let table_at = usize::try_from(13 + 76 + 1052 + sectors_len).unwrap();
        for b in &mut img[table_at + 76 + 24..table_at + 76 + 32] {
            *b ^= 0x77;
        }
        let p = file_in(dir.path(), "m.E01", &img);
        let (src, _) = open_image(&p, 512, None).unwrap();
        let all = read_all(src.as_ref());
        assert_eq!(&all[..4096], &pattern(4096)[..]);
        assert_eq!(&all[4096..], &[2u8; 4096][..]);
    }

    #[test]
    fn segment_names_advance_like_encase() {
        assert_eq!(next_extension("E01").as_deref(), Some("E02"));
        assert_eq!(next_extension("E99").as_deref(), Some("EAA"));
        assert_eq!(next_extension("EAZ").as_deref(), Some("EBA"));
        assert_eq!(next_extension("EZZ").as_deref(), Some("FAA"));
        assert_eq!(next_extension("e09").as_deref(), Some("e10"));
        assert_eq!(next_extension("ZZZ"), None);
    }

    #[test]
    fn adler32_matches_the_reference_value() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }
}
