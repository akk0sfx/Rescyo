//! LZX as used by WOF and WIM ([MS-PATCH] describes the format): every chunk is compressed on
//! its own with a window the size of the chunk, starts with fresh trees and repeat offsets, and
//! has x86 call translation applied with a fixed file size of 12 000 000.
//!
//! The bit stream is little-endian 16-bit words read most significant bit first. The reader
//! refills exactly as much as each read requests, which matters for the 16-bit realignment
//! before uncompressed blocks.

use crate::huffman::Huffman;
use crate::{CodecError, Result, copy_match, le16, le32};

const MIN_MATCH: usize = 2;
const NUM_CHARS: usize = 256;
const LENGTH_SYMS: usize = 249;
const PRETREE_SYMS: usize = 20;
const ALIGNED_SYMS: usize = 8;
const MAX_CODE: u32 = 16;
const E8_FILE_SIZE: i32 = 12_000_000;
const DEFAULT_BLOCK: usize = 32768;

const EXTRA_BITS: [u32; 50] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14,
    15, 15, 16, 16, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17,
];

fn slot_base(slot: usize) -> usize {
    let mut base = 0usize;
    for s in 0..slot {
        base += 1usize << EXTRA_BITS.get(s).copied().unwrap_or(17);
    }
    base
}

/// Offset slots for a window of `1 << order` bytes.
fn num_slots(order: u32) -> usize {
    match order {
        0..=15 => 30,
        16 => 32,
        17 => 34,
        18 => 36,
        19 => 38,
        20 => 42,
        _ => 50,
    }
}

struct Bits<'a> {
    input: &'a [u8],
    pos: usize,
    buf: u32,
    left: u32,
    overrun: u32,
}

impl<'a> Bits<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            buf: 0,
            left: 0,
            overrun: 0,
        }
    }

    fn ensure(&mut self, n: u32) -> Result<()> {
        while self.left < n {
            let w = match le16(self.input, self.pos) {
                Some(w) => w,
                None => {
                    // A few zero words past the end are normal look-ahead; more is a broken stream.
                    self.overrun += 1;
                    if self.overrun > 4 {
                        return Err(CodecError::Truncated);
                    }
                    0
                }
            };
            self.pos += 2;
            self.buf |= u32::from(w) << (16 - self.left);
            self.left += 16;
        }
        Ok(())
    }

    fn peek(&self, n: u32) -> u32 {
        if n == 0 { 0 } else { self.buf >> (32 - n) }
    }

    fn consume(&mut self, n: u32) {
        self.buf = self.buf.checked_shl(n).unwrap_or(0);
        self.left -= n;
    }

    fn read(&mut self, n: u32) -> Result<u32> {
        if n > 16 {
            let hi = self.read(n - 16)?;
            return Ok((hi << 16) | self.read(16)?);
        }
        self.ensure(n)?;
        let v = self.peek(n);
        self.consume(n);
        Ok(v)
    }

    fn symbol(&mut self, h: &Huffman) -> Result<usize> {
        self.ensure(MAX_CODE)?;
        let (s, len) = h.decode(self.peek(h.max_len()))?;
        self.consume(len);
        Ok(usize::from(s))
    }

    fn align(&mut self) -> Result<()> {
        self.ensure(1)?;
        self.buf = 0;
        self.left = 0;
        Ok(())
    }

    fn raw(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self
            .input
            .get(self.pos..self.pos + n)
            .ok_or(CodecError::Truncated)?;
        self.pos += n;
        Ok(s)
    }
}

/// Reads code lengths `lens[range]` coded with a fresh pretree, as deltas from their old values.
fn read_lens(b: &mut Bits<'_>, lens: &mut [u8]) -> Result<()> {
    let mut pre = [0u8; PRETREE_SYMS];
    for p in &mut pre {
        *p = u8::try_from(b.read(4)?).unwrap_or(0);
    }
    let pretree = Huffman::new(&pre, MAX_CODE)?;
    let delta = |old: u8, sym: usize| u8::try_from((usize::from(old) + 17 - sym) % 17).unwrap_or(0);
    let mut i = 0usize;
    while i < lens.len() {
        let sym = b.symbol(&pretree)?;
        match sym {
            0..=16 => {
                let l = lens.get_mut(i).ok_or(CodecError::Corrupt("code lengths"))?;
                *l = delta(*l, sym);
                i += 1;
            }
            17 | 18 => {
                let run = if sym == 17 {
                    4 + b.read(4)?
                } else {
                    20 + b.read(5)?
                } as usize;
                for _ in 0..run {
                    if let Some(l) = lens.get_mut(i) {
                        *l = 0;
                    }
                    i += 1;
                }
            }
            19 => {
                let run = 4 + b.read(1)? as usize;
                let s = b.symbol(&pretree)?;
                if s > 16 {
                    return Err(CodecError::Corrupt("pretree run value"));
                }
                let v = delta(*lens.get(i).ok_or(CodecError::Corrupt("code lengths"))?, s);
                for _ in 0..run {
                    if let Some(l) = lens.get_mut(i) {
                        *l = v;
                    }
                    i += 1;
                }
            }
            _ => return Err(CodecError::Corrupt("pretree symbol")),
        }
    }
    Ok(())
}

/// Decompresses one chunk that expands to exactly `out_len` bytes (at most 2 MiB).
pub fn decompress(input: &[u8], out_len: usize) -> Result<Vec<u8>> {
    if out_len > 1 << 21 {
        return Err(CodecError::Corrupt("chunk larger than any LZX window"));
    }
    let order = usize::BITS - out_len.max(1 << 15).next_power_of_two().leading_zeros() - 1;
    let main_syms = NUM_CHARS + num_slots(order) * 8;
    let mut main_lens = vec![0u8; main_syms];
    let mut len_lens = vec![0u8; LENGTH_SYMS];
    let mut out: Vec<u8> = Vec::with_capacity(out_len);
    let mut rep = [1usize; 3];
    let mut b = Bits::new(input);

    while out.len() < out_len {
        let kind = b.read(3)?;
        let size = if b.read(1)? == 1 {
            DEFAULT_BLOCK
        } else {
            let mut s = b.read(16)? as usize;
            if order >= 16 {
                s = (s << 8) | b.read(8)? as usize;
            }
            s
        };
        if size == 0 {
            return Err(CodecError::Corrupt("empty block"));
        }
        let end = (out.len() + size).min(out_len);
        match kind {
            1 | 2 => {
                let aligned = if kind == 2 {
                    let mut a = [0u8; ALIGNED_SYMS];
                    for x in &mut a {
                        *x = u8::try_from(b.read(3)?).unwrap_or(0);
                    }
                    Some(Huffman::new(&a, 7)?)
                } else {
                    None
                };
                let (lit, rest) = main_lens.split_at_mut(NUM_CHARS);
                read_lens(&mut b, lit)?;
                read_lens(&mut b, rest)?;
                read_lens(&mut b, &mut len_lens)?;
                let main = Huffman::new(&main_lens, MAX_CODE)?;
                // A block whose matches never need the length tree may leave it empty.
                let length = Huffman::new(&len_lens, MAX_CODE)?;
                while out.len() < end {
                    let sym = b.symbol(&main)?;
                    if sym < NUM_CHARS {
                        out.push(u8::try_from(sym).unwrap_or(0));
                        continue;
                    }
                    let sym = sym - NUM_CHARS;
                    let mut len = (sym & 7) + MIN_MATCH;
                    if sym & 7 == 7 {
                        len += b.symbol(&length)?;
                    }
                    let slot = sym >> 3;
                    let dist = match slot {
                        0 => rep[0],
                        1 => {
                            rep.swap(0, 1);
                            rep[0]
                        }
                        2 => {
                            rep.swap(0, 2);
                            rep[0]
                        }
                        _ => {
                            let extra = EXTRA_BITS.get(slot).copied().ok_or(CodecError::Corrupt("slot"))?;
                            let base = slot_base(slot);
                            let v = match &aligned {
                                Some(a) if extra >= 3 => {
                                    let hi = (b.read(extra - 3)? as usize) << 3;
                                    hi + b.symbol(a)?
                                }
                                _ => b.read(extra)? as usize,
                            };
                            let d = (base + v).checked_sub(2).ok_or(CodecError::Corrupt("offset"))?;
                            rep = [d, rep[0], rep[1]];
                            d
                        }
                    };
                    copy_match(&mut out, dist, len, out_len)?;
                }
            }
            3 => {
                b.align()?;
                for r in &mut rep {
                    *r = le32(b.raw(4)?, 0).ok_or(CodecError::Truncated)? as usize;
                }
                let n = end - out.len();
                out.extend_from_slice(b.raw(n)?);
                if size & 1 == 1 {
                    let _ = b.raw(1);
                }
            }
            _ => return Err(CodecError::Corrupt("block type")),
        }
    }
    out.truncate(out_len);
    undo_e8(&mut out);
    Ok(out)
}

/// Reverses the x86 CALL translation the compressor applied.
fn undo_e8(data: &mut [u8]) {
    if data.len() <= 10 {
        return;
    }
    let tail = data.len() - 10;
    let mut i = 0usize;
    while i < tail {
        if data.get(i) == Some(&0xE8) {
            if let Some(t) = data.get_mut(i + 1..i + 5) {
                let abs = le32(t, 0).map_or(0, u32::cast_signed);
                let pos = i32::try_from(i).unwrap_or(i32::MAX);
                let rel = if abs >= 0 {
                    (abs < E8_FILE_SIZE).then(|| abs - pos)
                } else {
                    (abs >= -pos).then(|| abs + E8_FILE_SIZE)
                };
                if let Some(r) = rel {
                    t.copy_from_slice(&r.to_le_bytes());
                }
            }
            i += 5;
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, reason = "test vectors")]
mod tests {
    use super::*;

    #[test]
    fn slot_bases_match_the_format() {
        let want = [
            0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768,
        ];
        for (s, &w) in want.iter().enumerate() {
            assert_eq!(slot_base(s), w, "slot {s}");
        }
        assert_eq!(slot_base(30), 32768);
    }

    #[test]
    fn an_uncompressed_block_decodes() {
        // Block type 3, default size bit 0, 16-bit size 5: bits 011 0 0000000000000101 → then
        // align, three repeat offsets, five bytes and one pad byte.
        let bits: u32 = 0b0110_0000_0000_0000_0101;
        let hi = (bits << 12) >> 16; // first 16 of the 20 bits
        let lo = ((bits & 0xF) << 12) as u16;
        let mut s = Vec::new();
        s.extend_from_slice(&(hi as u16).to_le_bytes());
        s.extend_from_slice(&lo.to_le_bytes());
        for _ in 0..3 {
            s.extend_from_slice(&1u32.to_le_bytes());
        }
        s.extend_from_slice(b"hello");
        s.push(0);
        assert_eq!(decompress(&s, 5).unwrap(), b"hello");
    }

    #[test]
    fn e8_translation_is_undone() {
        let mut d = vec![0u8; 32];
        d[4] = 0xE8;
        d[5..9].copy_from_slice(&(100i32 + 4).to_le_bytes());
        undo_e8(&mut d);
        assert_eq!(i32::from_le_bytes(d[5..9].try_into().unwrap()), 100);
    }

    #[test]
    fn hostile_streams_are_errors() {
        assert!(decompress(&[], 100).is_err());
        assert!(decompress(&[0xFF; 64], 100).is_err());
        assert!(decompress(&[0; 8], 1 << 22).is_err());
    }
}
