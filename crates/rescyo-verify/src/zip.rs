//! ZIP and ZIP-based documents (OOXML, ODF, EPUB, JAR, APK): the central directory must agree
//! with the local headers and every member must decompress to its recorded CRC-32 and size.
//! Decompression is capped against zip bombs.

use std::io;

use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};

use crate::{Input, Verdict, read_at};

const MAX_ENTRIES: u32 = 100_000;
const MAX_MEMBER: u64 = 1 << 30;
const MAX_RATIO: u64 = 1000;

fn u16le(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 2)
        .and_then(|s| <[u8; 2]>::try_from(s).ok())
        .map_or(0, |a| u64::from(u16::from_le_bytes(a)))
}

fn u32le(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map_or(0, |a| u64::from(u32::from_le_bytes(a)))
}

pub fn check(r: &mut dyn Input, len: u64) -> io::Result<Verdict> {
    // End of central directory: in the last 64 KiB + 22 bytes.
    let n = len.min(65_557);
    let mut tail = vec![0u8; usize::try_from(n).unwrap_or(0)];
    read_at(r, len - n, &mut tail)?;
    let Some(p) = tail.windows(4).rposition(|w| w == b"PK\x05\x06") else {
        return Ok(Verdict::damaged("no end of central directory"));
    };
    let eocd = tail.get(p..).unwrap_or_default();
    let entries = u16le(eocd, 10);
    let cd_size = u32le(eocd, 12);
    let cd_off = u32le(eocd, 16);
    if entries == 0xFFFF || cd_off == 0xFFFF_FFFF {
        return Ok(Verdict::Unverified); // ZIP64: not checked yet
    }
    if cd_off + cd_size > len || entries > u64::from(MAX_ENTRIES) {
        return Ok(Verdict::damaged("central directory out of range"));
    }
    let mut cd = vec![0u8; usize::try_from(cd_size).unwrap_or(0)];
    read_at(r, cd_off, &mut cd)?;
    let mut o = 0usize;
    for _ in 0..entries {
        if cd.get(o..o + 4) != Some(b"PK\x01\x02") {
            return Ok(Verdict::damaged("broken central directory"));
        }
        let method = u16le(&cd, o + 10);
        let crc = u32le(&cd, o + 16);
        let csize = u32le(&cd, o + 20);
        let usize_ = u32le(&cd, o + 24);
        let name_len = usize::try_from(u16le(&cd, o + 28)).unwrap_or(0);
        let extra_len = usize::try_from(u16le(&cd, o + 30)).unwrap_or(0);
        let comment_len = usize::try_from(u16le(&cd, o + 32)).unwrap_or(0);
        let local = u32le(&cd, o + 42);
        let flags = u16le(&cd, o + 8);
        let name =
            String::from_utf8_lossy(cd.get(o + 46..o + 46 + name_len).unwrap_or_default()).into_owned();
        o += 46 + name_len + extra_len + comment_len;
        if flags & 1 != 0 {
            return Ok(Verdict::Unverified); // encrypted
        }
        if usize_ > MAX_MEMBER || (csize > 0 && usize_ / csize.max(1) > MAX_RATIO) {
            return Ok(Verdict::Unverified);
        }
        let mut lh = [0u8; 30];
        if local + 30 > len {
            return Ok(Verdict::damaged(format!("{name}: local header out of range")));
        }
        read_at(r, local, &mut lh)?;
        if &lh[..4] != b"PK\x03\x04" {
            return Ok(Verdict::damaged(format!("{name}: missing local header")));
        }
        let data = local + 30 + u16le(&lh, 26) + u16le(&lh, 28);
        if data + csize > len {
            return Ok(Verdict::damaged(format!("{name}: data runs past the end")));
        }
        let mut comp = vec![0u8; usize::try_from(csize).unwrap_or(0)];
        read_at(r, data, &mut comp)?;
        let got = match method {
            0 => comp,
            8 => match inflate(&comp, usize_) {
                Some(v) => v,
                None => return Ok(Verdict::damaged(format!("{name}: deflate stream is broken"))),
            },
            _ => return Ok(Verdict::Unverified),
        };
        if got.len() as u64 != usize_ || u64::from(crc32fast::hash(&got)) != crc {
            return Ok(Verdict::damaged(format!("{name}: CRC mismatch")));
        }
    }
    Ok(Verdict::Intact)
}

/// Raw deflate with a hard output cap.
fn inflate(input: &[u8], expected: u64) -> Option<Vec<u8>> {
    let cap = usize::try_from(expected).ok()?;
    let mut out = vec![0u8; cap.max(1)];
    let mut d = DecompressorOxide::new();
    let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let (status, _, written) = decompress(&mut d, input, &mut out, 0, flags);
    if status != TINFLStatus::Done || written != cap {
        return None;
    }
    out.truncate(written);
    Some(out)
}
