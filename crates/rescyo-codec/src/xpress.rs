//! XPRESS Huffman ([MS-XCA] 2.2.4), the LZ77 + Huffman format of WOF's XPRESS4K/8K/16K.
//!
//! Each block of up to 64 KiB of output starts with 256 bytes holding 512 four-bit code lengths
//! (literals 0–255, then match symbols). The bit stream is read as little-endian 16-bit words,
//! most significant bit first, with extra match-length bytes taken from the byte stream between
//! the words; the reader below keeps exactly the look-ahead the specification describes, which
//! decides where those bytes come from.

use crate::huffman::Huffman;
use crate::{CodecError, Result, copy_match, le16};

const BLOCK: usize = 65536;
const MAX_LEN: u32 = 15;

struct Reader<'a> {
    input: &'a [u8],
    pos: usize,
    bits: u32,
    extra: i32,
}

impl<'a> Reader<'a> {
    fn new(input: &'a [u8], pos: usize) -> Result<Self> {
        let mut r = Self {
            input,
            pos,
            bits: 0,
            extra: 16,
        };
        let hi = r.word()?;
        let lo = r.word()?;
        r.bits = (u32::from(hi) << 16) | u32::from(lo);
        Ok(r)
    }

    fn word(&mut self) -> Result<u16> {
        let w = le16(self.input, self.pos).ok_or(CodecError::Truncated)?;
        self.pos += 2;
        Ok(w)
    }

    fn byte(&mut self) -> Result<u8> {
        let b = self.input.get(self.pos).copied().ok_or(CodecError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }

    fn consume(&mut self, n: u32) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        self.bits = self.bits.checked_shl(n).unwrap_or(0);
        self.extra -= i32::try_from(n).unwrap_or(32);
        if self.extra < 0 {
            // Words past the end read as zero: the stream may stop as soon as the output is
            // complete, and the output bound ends decoding.
            let w = self.word().unwrap_or(0);
            self.bits |= u32::from(w) << u32::try_from(-self.extra).unwrap_or(0);
            self.extra += 16;
        }
        Ok(())
    }
}

/// Decompresses one XPRESS Huffman stream that expands to exactly `out_len` bytes.
pub fn decompress(input: &[u8], out_len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(out_len.min(1 << 20));
    let mut table_at = 0usize;
    while out.len() < out_len {
        let table = input.get(table_at..table_at + 256).ok_or(CodecError::Truncated)?;
        let lens: Vec<u8> = table.iter().flat_map(|&b| [b & 0x0F, b >> 4]).collect();
        let code = Huffman::new(&lens, MAX_LEN)?;
        let mut r = Reader::new(input, table_at + 256)?;
        let block_end = (out.len() + BLOCK).min(out_len);
        while out.len() < block_end {
            let (sym, len) = code.decode(r.bits >> (32 - MAX_LEN))?;
            r.consume(len)?;
            if sym < 256 {
                out.push(u8::try_from(sym).unwrap_or(0));
                continue;
            }
            let sym = usize::from(sym - 256);
            let mut length = sym & 15;
            let offset_bits = u32::try_from(sym >> 4).unwrap_or(0);
            if length == 15 {
                length = usize::from(r.byte()?);
                if length == 255 {
                    let w = usize::from(u16::from_le_bytes([r.byte()?, r.byte()?]));
                    if w < 15 {
                        return Err(CodecError::Corrupt("match length"));
                    }
                    length = w - 15;
                }
                length += 15;
            }
            length += 3;
            let offset = if offset_bits == 0 {
                1
            } else {
                let v = (r.bits >> (32 - offset_bits)) as usize;
                r.consume(offset_bits)?;
                v + (1usize << offset_bits)
            };
            copy_match(&mut out, offset, length, out_len)?;
        }
        table_at = r.pos;
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, reason = "test encoder")]
pub(crate) mod tests {
    use super::*;

    /// Writes bits MSB-first into little-endian 16-bit words, with raw bytes interleaved the way
    /// [MS-XCA] 2.2.3 describes for the encoder.
    struct Writer {
        out: Vec<u8>,
        word_at: [usize; 2],
        bits: u32,
        free: u32,
    }

    impl Writer {
        fn new() -> Self {
            let mut out = vec![0u8; 4];
            out.truncate(4);
            Self {
                out,
                word_at: [0, 2],
                bits: 0,
                free: 16,
            }
        }
        fn put(&mut self, code: u32, n: u32) {
            if n == 0 {
                return;
            }
            if n <= self.free {
                self.bits = (self.bits << n) | code;
                self.free -= n;
            } else {
                let spill = n - self.free;
                self.bits = (self.bits << self.free) | (code >> spill);
                self.flush_word();
                self.bits = code & ((1 << spill) - 1);
                self.free = 16 - spill;
            }
        }
        fn flush_word(&mut self) {
            let at = self.word_at[0];
            self.out[at..at + 2].copy_from_slice(&(self.bits as u16).to_le_bytes());
            self.word_at[0] = self.word_at[1];
            self.word_at[1] = self.out.len();
            self.out.extend_from_slice(&[0, 0]);
        }
        fn byte(&mut self, b: u8) {
            self.out.push(b);
        }
        fn finish(mut self) -> Vec<u8> {
            self.bits <<= self.free;
            let at = self.word_at[0];
            self.out[at..at + 2].copy_from_slice(&(self.bits as u16).to_le_bytes());
            self.out
        }
    }

    /// A small compressor (greedy matches, fixed 9-bit-ish code lengths) for the tests.
    pub(crate) fn compress(data: &[u8]) -> Vec<u8> {
        assert!(data.len() <= BLOCK);
        // Tokens first.
        enum T {
            Lit(u8),
            Match(usize, usize),
        }
        let mut toks = Vec::new();
        let mut p = 0;
        while p < data.len() {
            let mut best = (0, 0);
            for d in 1..=p.min(8192) {
                let mut l = 0;
                while p + l < data.len() && l < 300 && data[p + l] == data[p + l - d] {
                    l += 1;
                }
                if l > best.0 {
                    best = (l, d);
                }
            }
            if best.0 >= 3 {
                toks.push(T::Match(best.0, best.1));
                p += best.0;
            } else {
                toks.push(T::Lit(data[p]));
                p += 1;
            }
        }
        // Complete code: 512 symbols of length 9.
        let lens = [9u8; 512];
        let mut out = Vec::new();
        for i in 0..256 {
            out.push(lens[2 * i] | (lens[2 * i + 1] << 4));
        }
        let mut w = Writer::new();
        for t in toks {
            match t {
                T::Lit(b) => w.put(u32::from(b), 9),
                T::Match(len, dist) => {
                    let ob = usize::BITS - 1 - dist.leading_zeros();
                    let l = len - 3;
                    let sym = 256 + ((ob as usize) << 4) + l.min(15);
                    w.put(sym as u32, 9);
                    if l >= 15 {
                        if l - 15 < 255 {
                            w.byte((l - 15) as u8);
                        } else {
                            w.byte(255);
                            let v = (l + 3 - 3) as u16; // stored as length - 3 when >= 255+15
                            w.byte(v.to_le_bytes()[0]);
                            w.byte(v.to_le_bytes()[1]);
                        }
                    }
                    w.put((dist - (1 << ob)) as u32, ob);
                }
            }
        }
        out.extend(w.finish());
        out
    }

    #[test]
    fn round_trips_literals_and_matches() {
        let mut data = b"WOF keeps Windows small; XPRESS keeps it fast. ".repeat(60);
        data.extend((0..1500u32).map(|i| (i.wrapping_mul(40503) >> 7) as u8));
        data.extend(std::iter::repeat_n(0u8, 600));
        let c = compress(&data);
        assert!(c.len() < data.len());
        assert_eq!(decompress(&c, data.len()).unwrap(), data);
    }

    #[test]
    fn hostile_streams_are_errors() {
        assert_eq!(decompress(&[0u8; 10], 5), Err(CodecError::Truncated));
        // All lengths zero: no code at all.
        assert!(decompress(&[0u8; 300], 5).is_err());
        // Over-subscribed: every symbol of length 1.
        assert!(decompress(&[0x11u8; 300], 5).is_err());
    }
}
