//! bzip2 decompression (DMG UDBZ chunks), written from the format as bzip2 1.0 writes it:
//! Huffman-coded MTF/RLE2 symbols, the inverse Burrows–Wheeler transform, then the initial
//! run-length step. Every block's CRC is checked, so damage is reported, never passed on.

use crate::huffman::Huffman;
use crate::{CodecError, Result};

const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
const END_MAGIC: u64 = 0x1772_4538_5090;
const MAX_GROUPS: usize = 6;
const MAX_CODE: u32 = 20;
const GROUP_SIZE: usize = 50;

struct Bits<'a> {
    b: &'a [u8],
    pos: usize, // in bits
}

impl Bits<'_> {
    fn read(&mut self, n: u32) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.b.get(self.pos / 8).ok_or(CodecError::Truncated)?;
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | u32::from(bit);
            self.pos += 1;
        }
        Ok(v)
    }

    fn bit(&mut self) -> Result<bool> {
        Ok(self.read(1)? == 1)
    }

    /// The next `n` bits without consuming them (zeros past the end).
    fn peek(&self, n: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..n as usize {
            let p = self.pos + i;
            let bit = self.b.get(p / 8).map_or(0, |byte| (byte >> (7 - (p % 8))) & 1);
            v = (v << 1) | u32::from(bit);
        }
        v
    }

    fn symbol(&mut self, h: &Huffman) -> Result<usize> {
        let (s, len) = h.decode(self.peek(h.max_len()))?;
        self.pos += len as usize;
        if self.pos > self.b.len() * 8 {
            return Err(CodecError::Truncated);
        }
        Ok(usize::from(s))
    }
}

fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (i, e) in t.iter_mut().enumerate() {
        let mut c = u32::try_from(i).unwrap_or(0) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
        }
        *e = c;
    }
    t
}

struct Crc<'a> {
    t: &'a [u32; 256],
    v: u32,
}

impl Crc<'_> {
    fn push(&mut self, b: u8) {
        let i = usize::from(self.v.to_be_bytes()[0] ^ b);
        self.v = (self.v << 8) ^ self.t.get(i).copied().unwrap_or(0);
    }
}

/// Decompresses a whole bzip2 stream, producing at most `out_len` bytes.
pub fn decompress(input: &[u8], out_len: usize) -> Result<Vec<u8>> {
    if input.get(0..3) != Some(b"BZh") {
        return Err(CodecError::Corrupt("not a bzip2 stream"));
    }
    let level = input.get(3).copied().unwrap_or(0);
    if !(b'1'..=b'9').contains(&level) {
        return Err(CodecError::Corrupt("block size"));
    }
    let max_block = usize::from(level - b'0') * 100_000;
    let crc_t = crc_table();
    let mut b = Bits { b: input, pos: 32 };
    let mut out = Vec::with_capacity(out_len.min(1 << 20));
    loop {
        let magic = (u64::from(b.read(24)?) << 24) | u64::from(b.read(24)?);
        if magic == END_MAGIC {
            break;
        }
        if magic != BLOCK_MAGIC {
            return Err(CodecError::Corrupt("block magic"));
        }
        let want_crc = b.read(32)?;
        if b.bit()? {
            return Err(CodecError::Corrupt("randomised blocks are obsolete"));
        }
        let orig_ptr = b.read(24)? as usize;
        // Byte values in use.
        let used16 = b.read(16)?;
        let mut seq = Vec::with_capacity(256);
        for i in 0..16u32 {
            if used16 & (0x8000 >> i) != 0 {
                let bits = b.read(16)?;
                for j in 0..16u32 {
                    if bits & (0x8000 >> j) != 0 {
                        seq.push(u8::try_from(i * 16 + j).unwrap_or(0));
                    }
                }
            }
        }
        if seq.is_empty() {
            return Err(CodecError::Corrupt("no symbols"));
        }
        let alpha = seq.len() + 2;
        let groups = b.read(3)? as usize;
        if !(2..=MAX_GROUPS).contains(&groups) {
            return Err(CodecError::Corrupt("tables"));
        }
        let selectors = b.read(15)? as usize;
        if selectors == 0 {
            return Err(CodecError::Corrupt("selectors"));
        }
        let mut mtf_g: Vec<u8> = (0..u8::try_from(groups).unwrap_or(6)).collect();
        let mut sel = Vec::with_capacity(selectors.min(18_002));
        for _ in 0..selectors {
            let mut j = 0usize;
            while b.bit()? {
                j += 1;
                if j >= groups {
                    return Err(CodecError::Corrupt("selector"));
                }
            }
            let v = mtf_g.remove(j);
            mtf_g.insert(0, v);
            // Only the first 18002 selectors can be used (900k / 50).
            if sel.len() < 18_002 {
                sel.push(v);
            }
        }
        let mut tables = Vec::with_capacity(groups);
        for _ in 0..groups {
            let mut len = b.read(5)? as i32;
            let mut lens = vec![0u8; alpha];
            for l in &mut lens {
                loop {
                    if !(1..=20).contains(&len) {
                        return Err(CodecError::Corrupt("code length"));
                    }
                    if !b.bit()? {
                        break;
                    }
                    len += if b.bit()? { -1 } else { 1 };
                }
                *l = u8::try_from(len).unwrap_or(0);
            }
            tables.push(Huffman::new(&lens, MAX_CODE)?);
        }
        // Symbols → MTF indices with RUNA/RUNB runs → bytes (pre-BWT).
        let mut mtf: Vec<u8> = (0..=255u8).collect();
        let mut tt: Vec<u8> = Vec::with_capacity(max_block);
        let (mut run, mut run_bit) = (0usize, 1usize);
        let mut count = 0usize;
        let mut group = 0usize;
        loop {
            if count.is_multiple_of(GROUP_SIZE) {
                group = usize::from(
                    *sel.get(count / GROUP_SIZE)
                        .ok_or(CodecError::Corrupt("selector index"))?,
                );
            }
            count += 1;
            let t = tables.get(group).ok_or(CodecError::Corrupt("table"))?;
            let s = b.symbol(t)?;
            if s <= 1 {
                run += run_bit << s;
                run_bit <<= 1;
                if run > max_block {
                    return Err(CodecError::Corrupt("run"));
                }
                continue;
            }
            if run > 0 {
                let v = *seq
                    .get(usize::from(mtf.first().copied().unwrap_or(0)))
                    .ok_or(CodecError::Corrupt("symbol"))?;
                if tt.len() + run > max_block {
                    return Err(CodecError::Corrupt("block too large"));
                }
                tt.extend(std::iter::repeat_n(v, run));
                run = 0;
                run_bit = 1;
            }
            if s == alpha - 1 {
                break;
            }
            let idx = s - 1;
            if idx >= seq.len() {
                return Err(CodecError::Corrupt("mtf index"));
            }
            let v = mtf.remove(idx);
            mtf.insert(0, v);
            if tt.len() >= max_block {
                return Err(CodecError::Corrupt("block too large"));
            }
            tt.push(*seq.get(usize::from(v)).ok_or(CodecError::Corrupt("symbol"))?);
        }
        if orig_ptr >= tt.len() {
            return Err(CodecError::Corrupt("origin pointer"));
        }
        // Inverse BWT: `next` links each byte to the one that follows it in the original text.
        let mut starts = [0usize; 256];
        for &c in &tt {
            if let Some(n) = starts.get_mut(usize::from(c)) {
                *n += 1;
            }
        }
        let mut sum = 0;
        for s in &mut starts {
            let n = *s;
            *s = sum;
            sum += n;
        }
        let mut next = vec![0u32; tt.len()];
        for (i, &c) in tt.iter().enumerate() {
            let s = starts.get_mut(usize::from(c)).ok_or(CodecError::Corrupt("bwt"))?;
            *next.get_mut(*s).ok_or(CodecError::Corrupt("bwt"))? = u32::try_from(i).unwrap_or(0);
            *s += 1;
        }
        let mut crc = Crc {
            t: &crc_t,
            v: u32::MAX,
        };
        let mut p = orig_ptr;
        // Undo the initial run-length step while emitting.
        let (mut last, mut same) = (None::<u8>, 0u32);
        for _ in 0..tt.len() {
            p = *next.get(p).ok_or(CodecError::Corrupt("bwt"))? as usize;
            let c = *tt.get(p).ok_or(CodecError::Corrupt("bwt"))?;
            if same == 4 {
                if let Some(l) = last {
                    for _ in 0..c {
                        crc.push(l);
                        if out.len() < out_len {
                            out.push(l);
                        }
                    }
                }
                same = 0;
                last = None;
                continue;
            }
            same = if last == Some(c) { same + 1 } else { 1 };
            last = Some(c);
            crc.push(c);
            if out.len() < out_len {
                out.push(c);
            }
        }
        let crc = crc.v;
        if !crc != want_crc {
            return Err(CodecError::Corrupt("block CRC"));
        }
        if out.len() >= out_len {
            break;
        }
    }
    Ok(out)
}
