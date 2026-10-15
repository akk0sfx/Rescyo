//! Where a file ends, found by walking its own structure.
//!
//! Each function starts at a file's first byte and returns its length, or `None` when the
//! structure breaks before a proper end: a carved file is reported only when its format says
//! where it stops. Every walk is bounded by the format's size cap and by item counts.

use crate::Format;
use crate::cursor::Cursor;

/// Most items (segments, chunks, atoms, records) followed in one file.
const MAX_ITEMS: u32 = 1 << 20;

pub fn length(c: &mut Cursor<'_>, format: Format) -> Option<u64> {
    let len = match format {
        Format::Jpeg => jpeg(c),
        Format::Png => png(c),
        Format::Gif => gif(c),
        Format::Pdf => pdf(c),
        Format::Zip => zip(c),
        Format::Mp4 => mp4(c),
        Format::Riff => riff(c),
        Format::Bmp => bmp(c),
    }?;
    (len > 0 && len <= format.max_size()).then_some(len)
}

fn be16(b: &[u8]) -> u64 {
    u64::from(u16::from_be_bytes([
        b.first().copied().unwrap_or(0),
        b.get(1).copied().unwrap_or(0),
    ]))
}

fn be32(b: &[u8]) -> u64 {
    let mut a = [0u8; 4];
    for (d, s) in a.iter_mut().zip(b) {
        *d = *s;
    }
    u64::from(u32::from_be_bytes(a))
}

fn le16(b: &[u8]) -> u64 {
    u64::from(u16::from_le_bytes([
        b.first().copied().unwrap_or(0),
        b.get(1).copied().unwrap_or(0),
    ]))
}

fn le32(b: &[u8]) -> u64 {
    let mut a = [0u8; 4];
    for (d, s) in a.iter_mut().zip(b) {
        *d = *s;
    }
    u64::from(u32::from_le_bytes(a))
}

fn le64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    for (d, s) in a.iter_mut().zip(b) {
        *d = *s;
    }
    u64::from_le_bytes(a)
}

/// JPEG: segments up to the first scan, entropy-coded data to the next marker, and so on until
/// the end-of-image marker (progressive files have several scans).
fn jpeg(c: &mut Cursor<'_>) -> Option<u64> {
    let mut pos = 2u64;
    for _ in 0..MAX_ITEMS {
        // Markers may be preceded by fill bytes.
        if c.byte(pos)? != 0xFF {
            return None;
        }
        while c.byte(pos + 1)? == 0xFF {
            pos += 1;
        }
        let m = c.byte(pos + 1)?;
        match m {
            0xD9 => return Some(pos + 2),
            0xD0..=0xD7 | 0x01 => pos += 2,
            0x00 => return None,
            _ => {
                let len = be16(&c.bytes(pos + 2, 2)?);
                if len < 2 {
                    return None;
                }
                pos += 2 + len;
                if m == 0xDA {
                    pos = scan_entropy(c, pos)?;
                }
            }
        }
    }
    None
}

/// Skips entropy-coded data: returns the position of the next real marker.
fn scan_entropy(c: &mut Cursor<'_>, mut pos: u64) -> Option<u64> {
    loop {
        pos = c.find_byte(pos, 0xFF)?;
        let next = c.byte(pos + 1)?;
        match next {
            0x00 | 0xD0..=0xD7 | 0xFF => pos += if next == 0xFF { 1 } else { 2 },
            _ => return Some(pos),
        }
    }
}

/// PNG: chunks until IEND, which must follow IHDR first.
fn png(c: &mut Cursor<'_>) -> Option<u64> {
    let mut pos = 8u64;
    for i in 0..MAX_ITEMS {
        let h = c.bytes(pos, 8)?;
        let len = be32(&h);
        let kind = h.get(4..8)?;
        if len > 1 << 31 || !kind.iter().all(u8::is_ascii_alphabetic) || (i == 0 && kind != b"IHDR") {
            return None;
        }
        pos += 12 + len;
        if kind == b"IEND" {
            return Some(pos);
        }
    }
    None
}

/// GIF: header, colour table, then image and extension blocks up to the trailer.
fn gif(c: &mut Cursor<'_>) -> Option<u64> {
    let flags = c.byte(10)?;
    let mut pos = 13u64;
    if flags & 0x80 != 0 {
        pos += 3 << ((flags & 7) + 1);
    }
    for _ in 0..MAX_ITEMS {
        match c.byte(pos)? {
            0x3B => return Some(pos + 1),
            0x21 => pos = sub_blocks(c, pos + 2)?,
            0x2C => {
                let f = c.byte(pos + 9)?;
                pos += 10;
                if f & 0x80 != 0 {
                    pos += 3 << ((f & 7) + 1);
                }
                pos = sub_blocks(c, pos + 1)?; // after the LZW minimum code size
            }
            _ => return None,
        }
    }
    None
}

fn sub_blocks(c: &mut Cursor<'_>, mut pos: u64) -> Option<u64> {
    for _ in 0..MAX_ITEMS {
        let n = c.byte(pos)?;
        pos += 1 + u64::from(n);
        if n == 0 {
            return Some(pos);
        }
    }
    None
}

/// PDF: up to `%%EOF`, following incremental updates (more objects or a new cross-reference
/// section right after it).
fn pdf(c: &mut Cursor<'_>) -> Option<u64> {
    let mut pos = 5u64;
    let mut end = None;
    for _ in 0..4096 {
        let Some(at) = c.find(pos, b"%%EOF") else { break };
        let mut after = at + 5;
        while matches!(c.byte(after), Some(b'\r' | b'\n' | b' ' | b'\t')) {
            after += 1;
        }
        end = Some(after);
        let next = c.bytes(after, 24).unwrap_or_default();
        let continues = next.starts_with(b"xref")
            || next.first().is_some_and(u8::is_ascii_digit) && next.windows(3).any(|w| w == b"obj");
        if !continues {
            break;
        }
        pos = after;
    }
    end
}

/// ZIP (and OOXML, ODF, EPUB, JAR, APK): local entries, the central directory, then the end
/// record with its comment.
fn zip(c: &mut Cursor<'_>) -> Option<u64> {
    let mut pos = 0u64;
    for _ in 0..MAX_ITEMS {
        let sig = c.bytes(pos, 4)?;
        match sig.as_slice() {
            b"PK\x03\x04" => {
                let h = c.bytes(pos, 30)?;
                let flags = le16(h.get(6..)?);
                let csize = le32(h.get(18..)?);
                let skip = 30 + le16(h.get(26..)?) + le16(h.get(28..)?);
                if flags & 8 == 0 && csize != 0xFFFF_FFFF {
                    pos += skip + csize;
                } else {
                    // Sizes follow the data: the next record says where it ended.
                    pos = next_zip_record(c, pos + skip)?;
                }
            }
            b"PK\x07\x08" => pos += 16,
            b"PK\x01\x02" => {
                let h = c.bytes(pos, 46)?;
                pos += 46 + le16(h.get(28..)?) + le16(h.get(30..)?) + le16(h.get(32..)?);
            }
            b"PK\x06\x06" => pos += 12 + le64(&c.bytes(pos + 4, 8)?),
            b"PK\x06\x07" => pos += 20,
            b"PK\x05\x06" => return Some(pos + 22 + le16(&c.bytes(pos + 20, 2)?)),
            _ => return None,
        }
    }
    None
}

fn next_zip_record(c: &mut Cursor<'_>, from: u64) -> Option<u64> {
    let mut pos = from;
    loop {
        let at = c.find(pos, b"PK")?;
        let sig = c.bytes(at, 4)?;
        if matches!(sig.as_slice(), b"PK\x07\x08" | b"PK\x03\x04" | b"PK\x01\x02") {
            return Some(at);
        }
        pos = at + 1;
    }
}

/// MP4/MOV/3GP/HEIF: top-level atoms of known kinds, starting with `ftyp`; the file ends where
/// they stop. A movie without `moov` or `mdat` is not complete enough to report.
fn mp4(c: &mut Cursor<'_>) -> Option<u64> {
    const TOP: [&[u8; 4]; 16] = [
        b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"uuid", b"meta", b"pdin", b"moof", b"mfra",
        b"sidx", b"styp", b"emsg", b"prft", b"pnot",
    ];
    let mut pos = 0u64;
    let (mut moov, mut mdat) = (false, false);
    for i in 0..MAX_ITEMS {
        let Some(h) = c.bytes(pos, 16) else { break };
        let kind = h.get(4..8)?;
        if !TOP.iter().any(|t| t.as_slice() == kind) || (i == 0 && kind != b"ftyp") {
            break;
        }
        let size = match be32(&h) {
            1 => {
                let hi = be32(h.get(8..)?);
                let lo = be32(h.get(12..)?);
                (hi << 32) | lo
            }
            0 => return None, // "to the end of the file": no end to find
            s => s,
        };
        if size < 8 {
            return None;
        }
        moov |= kind == b"moov";
        mdat |= kind == b"mdat";
        pos += size;
    }
    (pos > 0 && (moov || mdat)).then_some(pos)
}

/// RIFF (WAV, AVI, WEBP): the size field.
fn riff(c: &mut Cursor<'_>) -> Option<u64> {
    let size = le32(&c.bytes(4, 4)?);
    if size < 4 {
        return None;
    }
    Some(8 + size + (size & 1))
}

/// BMP: the size field, checked against the header.
fn bmp(c: &mut Cursor<'_>) -> Option<u64> {
    let h = c.bytes(0, 18)?;
    let size = le32(h.get(2..)?);
    let reserved = le32(h.get(6..)?);
    let pixels = le32(h.get(10..)?);
    let dib = le32(h.get(14..)?);
    let ok = reserved == 0
        && size >= 26
        && pixels >= 14 + dib
        && pixels < size
        && matches!(dib, 12 | 40 | 52 | 56 | 108 | 124);
    ok.then_some(size)
}
