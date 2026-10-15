//! Apple disk images (UDIF `.dmg`): read-only, compressed or not.
//!
//! The image ends with a 512-byte `koly` trailer that points at an XML property list. Its
//! `blkx` array holds one `mish` table per partition, base64-encoded; each table lists runs of
//! sectors and where they are stored: zero-filled, raw, or compressed with ADC (UDCO), zlib
//! (UDZO), bzip2 (UDBZ), LZFSE (ULFO) or LZMA in xz (ULMO). A run that cannot be decoded makes
//! only its own sectors unreadable.
//!
//! Not supported: encrypted images, segmented images (`.dmgpart`), sparse images and bundles,
//! and images that keep their tables only in a classic resource fork.

use super::{ContainerFormat, HostFile, Layout, OpenError, Opened, Piece, inflate_zlib};
use crate::ReadError;
use rescyo_codec::{adc, bzip2, lzfse, xz};
use std::path::Path;

const FORMAT: ContainerFormat = ContainerFormat::Dmg;
pub(crate) const TRAILER: &[u8; 4] = b"koly";
pub(crate) const ENCRYPTED: &[u8; 8] = b"encrcdsa";
pub(crate) const SPARSE: &[u8; 4] = b"sprs";
const SECTOR: u64 = 512;
/// Larger property lists are not something hdiutil writes.
const MAX_XML: u64 = 64 << 20;
/// Runs larger than this (decoded or stored compressed) are treated as damage.
const MAX_RUN: u64 = 64 << 20;
const MAX_RUNS: usize = 1 << 22;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Zero,
    Raw,
    Adc,
    Zlib,
    Bzip2,
    Lzfse,
    Xz,
    /// A chunk type this reader does not know: unreadable, not zero.
    Unknown,
}

impl Kind {
    fn from_type(t: u32) -> Option<Self> {
        Some(match t {
            0 | 2 => Kind::Zero,
            1 => Kind::Raw,
            0x8000_0004 => Kind::Adc,
            0x8000_0005 => Kind::Zlib,
            0x8000_0006 => Kind::Bzip2,
            0x8000_0007 => Kind::Lzfse,
            0x8000_0008 => Kind::Xz,
            // Comments and the end marker describe no sectors.
            0x7FFF_FFFE | 0xFFFF_FFFF => return None,
            _ => Kind::Unknown,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct Run {
    /// Virtual byte range.
    start: u64,
    len: u64,
    kind: Kind,
    /// Stored bytes in the image file.
    src: u64,
    src_len: u64,
}

#[derive(Debug)]
struct DmgLayout {
    size: u64,
    runs: Vec<Run>,
}

impl Layout for DmgLayout {
    fn size(&self) -> u64 {
        self.size
    }

    fn locate(&self, _: &[HostFile], offset: u64) -> Result<(Piece, u64), ReadError> {
        let i = self.runs.partition_point(|r| r.start + r.len <= offset);
        let Some(run) = self.runs.get(i).filter(|r| r.start <= offset) else {
            // Sectors no table describes read as zeros, as hdiutil does.
            let next = self.runs.get(i).map_or(self.size, |r| r.start);
            return Ok((Piece::Zero, next.min(self.size) - offset));
        };
        let within = offset - run.start;
        let left = (run.len - within).min(self.size - offset);
        let piece = match run.kind {
            Kind::Zero => Piece::Zero,
            Kind::Raw => Piece::Data {
                file: 0,
                offset: run.src + within,
            },
            Kind::Unknown => Piece::Bad,
            _ => Piece::Chunk {
                chunk: i as u64,
                within,
            },
        };
        Ok((piece, left))
    }

    fn decode(&self, files: &[HostFile], chunk: u64) -> Result<Vec<u8>, ReadError> {
        let run = usize::try_from(chunk)
            .ok()
            .and_then(|i| self.runs.get(i))
            .ok_or(ReadError::Media)?;
        let want = usize::try_from(run.len).map_err(|_| ReadError::Media)?;
        let mut raw = vec![0u8; usize::try_from(run.src_len).map_err(|_| ReadError::Media)?];
        files.first().ok_or(ReadError::Media)?.read(run.src, &mut raw)?;
        let out = match run.kind {
            Kind::Zlib => inflate_zlib(&raw, want)?,
            Kind::Adc => adc::decompress(&raw, want).map_err(|_| ReadError::Media)?,
            Kind::Bzip2 => bzip2::decompress(&raw, want).map_err(|_| ReadError::Media)?,
            Kind::Lzfse => lzfse::decompress(&raw, want).map_err(|_| ReadError::Media)?,
            Kind::Xz => xz::decompress(&raw, want).map_err(|_| ReadError::Media)?,
            Kind::Zero | Kind::Raw | Kind::Unknown => return Err(ReadError::Media),
        };
        if out.len() != want {
            return Err(ReadError::Media);
        }
        Ok(out)
    }
}

fn be32(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u32::from_be_bytes)
}

fn be64(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u64::from_be_bytes)
}

/// Decodes base64, skipping whitespace (property lists wrap it at 72 columns).
fn base64(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in text {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).unwrap_or(0));
        }
    }
    Some(out)
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// The base64 `Data` blobs of the `blkx` array of a UDIF property list.
fn blkx_tables(xml: &[u8]) -> Option<Vec<Vec<u8>>> {
    let key = find(xml, b"<key>blkx</key>", 0)?;
    let open = find(xml, b"<array>", key)?;
    let close = find(xml, b"</array>", open)?;
    let mut out = Vec::new();
    let mut at = open;
    while let Some(s) = find(xml, b"<data>", at).filter(|&s| s < close) {
        let e = find(xml, b"</data>", s).filter(|&e| e <= close)?;
        out.push(base64(xml.get(s + 6..e)?)?);
        at = e;
    }
    Some(out)
}

/// The runs of one `mish` table; `base` is where its stored offsets count from.
fn mish_runs(m: &[u8], base: u64, file_len: u64, runs: &mut Vec<Run>, notes: &mut Vec<String>) {
    if m.get(..4) != Some(b"mish") {
        notes.push("a partition table entry is not a mish block; its sectors read as zeros".into());
        return;
    }
    let first = be64(m, 8);
    let base = base.saturating_add(be64(m, 24));
    let count = be32(m, 0xC8) as usize;
    let mut bad = 0usize;
    for i in 0..count {
        let at = 0xCC + 40 * i;
        let Some(e) = m.get(at..at + 40) else {
            notes.push("a mish block is truncated".into());
            break;
        };
        let Some(kind) = Kind::from_type(be32(e, 0)) else {
            continue;
        };
        let sector = first.saturating_add(be64(e, 8));
        let sectors = be64(e, 16);
        let src = base.saturating_add(be64(e, 24));
        let src_len = be64(e, 32);
        let (start, len) = (sector.saturating_mul(SECTOR), sectors.saturating_mul(SECTOR));
        let stored =
            matches!(kind, Kind::Zero) || src.checked_add(src_len).is_some_and(|end| end <= file_len);
        let sane = len > 0 && len <= MAX_RUN && src_len <= MAX_RUN && start.checked_add(len).is_some();
        if !sane {
            bad += 1;
            continue;
        }
        let kind = if stored { kind } else { Kind::Unknown };
        if runs.len() < MAX_RUNS {
            runs.push(Run {
                start,
                len,
                kind,
                src,
                src_len,
            });
        }
    }
    if bad > 0 {
        notes.push(format!("{bad} chunk entries are malformed and were skipped"));
    }
}

pub(crate) fn open(path: &Path) -> Result<Opened, OpenError> {
    let file = HostFile::open(path)?;
    let head = file.read_vec(0, 8.min(usize::try_from(file.len).unwrap_or(8)))?;
    if head.starts_with(ENCRYPTED) {
        return Err(OpenError::unsupported(FORMAT, "an encrypted disk image"));
    }
    if head.starts_with(SPARSE) {
        return Err(OpenError::unsupported(
            FORMAT,
            "a sparse disk image (.sparseimage)",
        ));
    }
    if file.len < 512 {
        return Err(OpenError::invalid(FORMAT, "file is shorter than the trailer"));
    }
    let koly = file.read_vec(file.len - 512, 512)?;
    if koly.get(..4) != Some(TRAILER) {
        return Err(OpenError::invalid(FORMAT, "no koly trailer"));
    }
    if be32(&koly, 60) > 1 {
        return Err(OpenError::unsupported(FORMAT, "a segmented disk image"));
    }
    let data_fork = be64(&koly, 24);
    let (xml_at, xml_len) = (be64(&koly, 0xD8), be64(&koly, 0xE0));
    if xml_len == 0 {
        return Err(OpenError::unsupported(
            FORMAT,
            "a disk image without an XML property list",
        ));
    }
    if xml_len > MAX_XML || xml_at.checked_add(xml_len).is_none_or(|e| e > file.len) {
        return Err(OpenError::invalid(FORMAT, "property list outside the file"));
    }
    let xml = file.read_vec(xml_at, usize::try_from(xml_len).unwrap_or(0))?;
    let tables = blkx_tables(&xml).ok_or_else(|| OpenError::invalid(FORMAT, "no readable blkx table"))?;
    let mut runs = Vec::new();
    let mut notes = Vec::new();
    for t in &tables {
        mish_runs(t, data_fork, file.len, &mut runs, &mut notes);
    }
    runs.sort_by_key(|r| r.start);
    // Overlapping runs: keep the first, as the tables were listed.
    let mut kept: Vec<Run> = Vec::with_capacity(runs.len());
    let mut overlaps = 0usize;
    for r in runs {
        if kept.last().is_some_and(|p| p.start + p.len > r.start) {
            overlaps += 1;
        } else {
            kept.push(r);
        }
    }
    if overlaps > 0 {
        notes.push(format!("{overlaps} overlapping chunk entries were ignored"));
    }
    let described = kept.last().map_or(0, |r| r.start + r.len);
    let declared = be64(&koly, 0x1EC).saturating_mul(SECTOR);
    let size = if declared == 0 { described } else { declared };
    if size == 0 {
        return Err(OpenError::invalid(FORMAT, "the image describes no sectors"));
    }
    if described > size {
        notes.push("chunk entries describe sectors past the declared size; they are ignored".into());
        kept.retain(|r| r.start < size);
    }
    Ok(Opened {
        files: vec![file],
        layout: Box::new(DmgLayout { size, runs: kept }),
        logical_sector: 512,
        parent: None,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testutil::file_in;
    use super::*;

    fn b64(data: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in data.chunks(3) {
            let n = c
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
            for i in 0..=c.len() {
                s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            }
            s.extend(std::iter::repeat_n('=', 3 - c.len()));
            if s.len() % 73 == 72 {
                s.push('\n');
            }
        }
        s
    }

    /// A UDIF image of eight sectors: raw, zlib (two sectors), zero, ignore, an unknown
    /// type, then two sectors no chunk describes.
    fn sample() -> (Vec<u8>, Vec<u8>) {
        let mut disk = vec![0u8; 8 * 512];
        for (i, b) in disk[..3 * 512].iter_mut().enumerate() {
            *b = u8::try_from(i * 7 % 251).unwrap() + 1;
        }
        let mut file = disk[..512].to_vec();
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&disk[512..3 * 512], 6);
        file.extend_from_slice(&z);
        let entry = |t: u32, sec: u64, n: u64, off: u64, len: u64| {
            let mut e = Vec::new();
            for v in [t, 0] {
                e.extend_from_slice(&v.to_be_bytes());
            }
            for v in [sec, n, off, len] {
                e.extend_from_slice(&v.to_be_bytes());
            }
            e
        };
        let entries = [
            entry(1, 0, 1, 0, 512),
            entry(0x8000_0005, 1, 2, 512, z.len() as u64),
            entry(0, 3, 1, 0, 0),
            entry(2, 4, 1, 0, 0),
            entry(0x7FFF_FFFE, 5, 0, 0, 0),
            entry(0x8000_0042, 5, 1, 0, 0),
            entry(0xFFFF_FFFF, 6, 0, 0, 0),
        ];
        let mut m = vec![0u8; 0xCC];
        m[..4].copy_from_slice(b"mish");
        m[4..8].copy_from_slice(&1u32.to_be_bytes());
        m[16..24].copy_from_slice(&6u64.to_be_bytes());
        m[0xC8..0xCC].copy_from_slice(&u32::try_from(entries.len()).unwrap().to_be_bytes());
        for e in &entries {
            m.extend_from_slice(e);
        }
        let xml = format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>resource-fork</key><dict>\
             <key>blkx</key><array><dict><key>Data</key><data>\n{}\n</data><key>Name</key>\
             <string>whole disk</string></dict></array></dict></dict></plist>",
            b64(&m)
        );
        let xml_at = file.len() as u64;
        file.extend_from_slice(xml.as_bytes());
        let mut koly = vec![0u8; 512];
        koly[..4].copy_from_slice(TRAILER);
        koly[4..8].copy_from_slice(&4u32.to_be_bytes());
        koly[8..12].copy_from_slice(&512u32.to_be_bytes());
        koly[32..40].copy_from_slice(&xml_at.to_be_bytes());
        koly[0xD8..0xE0].copy_from_slice(&xml_at.to_be_bytes());
        koly[0xE0..0xE8].copy_from_slice(&(xml.len() as u64).to_be_bytes());
        koly[0x1EC..0x1F4].copy_from_slice(&8u64.to_be_bytes());
        file.extend_from_slice(&koly);
        (file, disk)
    }

    #[test]
    fn reads_raw_zlib_and_zero_runs() {
        let dir = tempfile::tempdir().unwrap();
        let (file, disk) = sample();
        let p = file_in(dir.path(), "a.dmg", &file);
        let (src, info) = crate::open_image(&p, 512, None).unwrap();
        assert_eq!(info.format, ContainerFormat::Dmg);
        assert_eq!(src.geometry().size, 8 * 512);
        let mut buf = vec![0u8; 5 * 512];
        src.read_at(0, &mut buf).unwrap();
        assert_eq!(buf, disk[..5 * 512]);
        // The unknown chunk type is unreadable, not silently zero.
        assert!(src.read_at(5 * 512, &mut [0u8; 512]).is_err());
        let mut tail = vec![1u8; 1024];
        src.read_at(6 * 512, &mut tail).unwrap();
        assert!(tail.iter().all(|&b| b == 0));
    }

    #[test]
    fn damage_stays_local() {
        let dir = tempfile::tempdir().unwrap();
        let (mut file, disk) = sample();
        file[512 + 4] ^= 0xFF; // inside the zlib stream
        let p = file_in(dir.path(), "b.dmg", &file);
        let (src, _) = crate::open_image(&p, 512, None).unwrap();
        let mut s = vec![0u8; 512];
        src.read_at(0, &mut s).unwrap();
        assert_eq!(s, disk[..512]);
        assert!(src.read_at(512, &mut s).is_err());
    }

    #[test]
    fn rejects_encrypted_and_truncated_images() {
        let dir = tempfile::tempdir().unwrap();
        let p = file_in(dir.path(), "e.dmg", b"encrcdsa\0\0\0\x02");
        assert!(matches!(open(&p), Err(OpenError::Unsupported { .. })));
        let (file, _) = sample();
        let p = file_in(dir.path(), "t.dmg", &file[..file.len() - 600]);
        assert!(open(&p).is_err());
    }
}
