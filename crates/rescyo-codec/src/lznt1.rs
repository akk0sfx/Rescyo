//! LZNT1 ([MS-XCA] 2.5): NTFS compression of 4 KiB chunks inside each compression unit.
//!
//! A buffer is a series of chunks, each with a 16-bit header: the low 12 bits are the chunk
//! length minus 3, bit 15 says whether it is compressed. A compressed chunk is groups of a flag
//! byte and eight items: a literal byte or a 16-bit back-reference whose split between offset
//! and length depends on how far into the 4 KiB chunk the decoder is.

use crate::{CodecError, Result, copy_match, le16};

const CHUNK: usize = 4096;

/// Decompresses `input`, producing at most `out_len` bytes. A chunk that decodes to less than
/// 4 KiB is followed by zeros, as NTFS does; a buffer that ends early yields what it holds and
/// the caller pads.
pub fn decompress(input: &[u8], out_len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(out_len.min(1 << 20));
    let mut pos = 0usize;
    while out.len() < out_len {
        let Some(header) = le16(input, pos) else { break };
        if header == 0 {
            break;
        }
        let size = usize::from(header & 0x0FFF) + 3;
        let data = input.get(pos + 2..pos + size).ok_or(CodecError::Truncated)?;
        pos += size;
        let chunk_end = (out.len() + CHUNK).min(out_len);
        if header & 0x8000 == 0 {
            let n = data.len().min(chunk_end - out.len());
            out.extend_from_slice(data.get(..n).unwrap_or_default());
        } else {
            chunk(data, &mut out, chunk_end)?;
        }
        out.resize(chunk_end, 0);
    }
    Ok(out)
}

fn chunk(data: &[u8], out: &mut Vec<u8>, chunk_end: usize) -> Result<()> {
    let start = out.len();
    let mut i = 0usize;
    while i < data.len() && out.len() < chunk_end {
        let flags = data.get(i).copied().ok_or(CodecError::Truncated)?;
        i += 1;
        for bit in 0..8 {
            if i >= data.len() || out.len() >= chunk_end {
                break;
            }
            if flags >> bit & 1 == 0 {
                out.push(data.get(i).copied().ok_or(CodecError::Truncated)?);
                i += 1;
                continue;
            }
            let token = usize::from(le16(data, i).ok_or(CodecError::Truncated)?);
            i += 2;
            let p = out.len() - start;
            if p == 0 {
                return Err(CodecError::Corrupt("back-reference at the start of a chunk"));
            }
            let mut len_bits = 12u32;
            let mut x = p - 1;
            while x >= 0x10 {
                x >>= 1;
                len_bits -= 1;
            }
            let len = (token & ((1 << len_bits) - 1)) + 3;
            let dist = (token >> len_bits) + 1;
            if dist > p {
                return Err(CodecError::Corrupt("back-reference before the chunk"));
            }
            copy_match(out, dist, len, chunk_end)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, reason = "test encoder")]
mod tests {
    use super::*;

    /// A minimal greedy compressor, only to produce valid streams for the tests.
    pub(crate) fn compress(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for c in data.chunks(CHUNK) {
            let mut body = Vec::new();
            let mut p = 0;
            while p < c.len() {
                let flag_at = body.len();
                body.push(0u8);
                for bit in 0..8 {
                    if p >= c.len() {
                        break;
                    }
                    let mut len_bits = 12u32;
                    if p > 0 {
                        let mut x = p - 1;
                        while x >= 0x10 {
                            x >>= 1;
                            len_bits -= 1;
                        }
                    }
                    let max_len = ((1usize << len_bits) - 1 + 3).min(c.len() - p);
                    let max_dist = (1usize << (16 - len_bits)).min(p);
                    let mut best = (0, 0);
                    for d in 1..=max_dist {
                        let mut l = 0;
                        while l < max_len && c[p + l] == c[p + l - d] {
                            l += 1;
                        }
                        if l > best.0 {
                            best = (l, d);
                        }
                    }
                    if best.0 >= 3 {
                        let token = ((best.1 - 1) << len_bits) | (best.0 - 3);
                        body.extend_from_slice(&(token as u16).to_le_bytes());
                        body[flag_at] |= 1 << bit;
                        p += best.0;
                    } else {
                        body.push(c[p]);
                        p += 1;
                    }
                }
            }
            if body.len() < c.len() {
                out.extend_from_slice(&((0xB000 | (body.len() + 2 - 3)) as u16).to_le_bytes());
                out.extend_from_slice(&body);
            } else {
                out.extend_from_slice(&((0x3000 | (c.len() + 2 - 3)) as u16).to_le_bytes());
                out.extend_from_slice(c);
            }
        }
        out
    }

    #[test]
    fn round_trips_text_runs_and_incompressible_chunks() {
        let mut data = b"Rescyo restores folders. ".repeat(400);
        data.extend((0..5000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8));
        data.extend(std::iter::repeat_n(b'z', 3000));
        let c = compress(&data);
        assert!(c.len() < data.len());
        assert_eq!(decompress(&c, data.len()).unwrap(), data);
        // Asking for less stops early.
        assert_eq!(decompress(&c, 100).unwrap(), data[..100]);
    }

    #[test]
    fn short_chunks_are_padded_and_hostile_streams_rejected() {
        // One compressed chunk holding "abc" only: the rest of the 4 KiB is zeros.
        let c = [0x03, 0xB0, 0x00, b'a', b'b', b'c'];
        let d = decompress(&c, 5000).unwrap();
        assert_eq!(&d[..3], b"abc");
        assert_eq!(d.len(), 4096);
        assert!(d[3..].iter().all(|&b| b == 0));
        // A back-reference first.
        assert!(decompress(&[0x02, 0xB0, 0x01, 0x00, 0x00], 10).is_err());
        // A header promising more than there is.
        assert_eq!(decompress(&[0xFF, 0xBF, 0x00], 10), Err(CodecError::Truncated));
    }
}
