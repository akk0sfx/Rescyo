//! JPEG: the end marker, then a strict full decode.
//!
//! JPEG carries no checksum: truncation, zeroed sectors and foreign clusters break the decode,
//! but a few flipped bits inside the compressed data can decode into slightly wrong pixels
//! without any error. `Intact` means "decodes cleanly", not "bit-exact".

use std::io;

use zune_core::bytestream::ZCursor;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::{Input, Verdict, read_at};

/// Larger files are not decoded (memory), only structurally checked.
const MAX_DECODE: u64 = 128 << 20;

pub fn check(r: &mut dyn Input, len: u64) -> io::Result<Verdict> {
    if len < 4 {
        return Ok(Verdict::damaged("truncated"));
    }
    let mut tail = [0u8; 2];
    read_at(r, len - 2, &mut tail)?;
    // Some cameras pad after EOI; accept an EOI within the last 64 KiB.
    if tail != [0xFF, 0xD9] && !eoi_near_end(r, len)? {
        return Ok(Verdict::damaged("no end-of-image marker"));
    }
    if len > MAX_DECODE {
        return Ok(Verdict::Unverified);
    }
    let mut data = vec![0u8; usize::try_from(len).unwrap_or(0)];
    read_at(r, 0, &mut data)?;
    let opts = DecoderOptions::default()
        .set_strict_mode(true)
        .set_max_width(65_535)
        .set_max_height(65_535);
    let mut dec = JpegDecoder::new_with_options(ZCursor::new(&data), opts);
    match dec.decode() {
        Ok(_) => Ok(Verdict::Intact),
        Err(e) => Ok(Verdict::damaged(format!("decode failed: {e:?}"))),
    }
}

fn eoi_near_end(r: &mut dyn Input, len: u64) -> io::Result<bool> {
    let n = len.min(64 << 10);
    let mut b = vec![0u8; usize::try_from(n).unwrap_or(0)];
    read_at(r, len - n, &mut b)?;
    Ok(b.windows(2).any(|w| w == [0xFF, 0xD9]))
}
