//! xz streams with the LZMA2 filter (DMG ULMO chunks), written from the xz file format
//! specification and the LZMA specification in the LZMA SDK.
//!
//! The whole output stays in memory, bounded by the caller's expected size, so the LZMA
//! dictionary is simply the output since the last dictionary reset: nothing is allocated from
//! sizes the stream declares. Block checks (CRC32, CRC64) and the index are verified; SHA-256
//! checks are skipped (the LZMA2 structure and the index still have to agree).

use crate::{CodecError, Result};

const MAGIC: &[u8; 6] = b"\xFD7zXZ\0";
const FOOTER_MAGIC: &[u8; 2] = b"YZ";
const LZMA2_FILTER: u64 = 0x21;

fn crc32(data: &[u8]) -> u32 {
    let mut c = u32::MAX;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn crc64(data: &[u8]) -> u64 {
    let mut c = u64::MAX;
    for &b in data {
        c ^= u64::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xC96C_5795_D787_0F42
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn byte(b: &[u8], at: usize) -> Result<u8> {
    b.get(at).copied().ok_or(CodecError::Truncated)
}

fn le32(b: &[u8], at: usize) -> Result<u32> {
    crate::le32(b, at).ok_or(CodecError::Truncated)
}

fn be16(b: &[u8], at: usize) -> Result<usize> {
    Ok(usize::from(byte(b, at)?) << 8 | usize::from(byte(b, at + 1)?))
}

/// An xz variable-length integer: seven bits per byte, least significant first, at most nine.
fn varint(b: &[u8], at: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for i in 0..9 {
        let x = byte(b, *at)?;
        *at += 1;
        v |= u64::from(x & 0x7f) << (7 * i);
        if x & 0x80 == 0 {
            if i > 0 && x == 0 {
                return Err(CodecError::Corrupt("xz integer"));
            }
            return Ok(v);
        }
    }
    Err(CodecError::Corrupt("xz integer"))
}

/// The range decoder over one LZMA chunk.
struct Rc<'a> {
    b: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl<'a> Rc<'a> {
    fn new(b: &'a [u8]) -> Result<Self> {
        if byte(b, 0)? != 0 {
            return Err(CodecError::Corrupt("lzma range coder start"));
        }
        let code = u32::from_be_bytes(
            b.get(1..5)
                .ok_or(CodecError::Truncated)?
                .try_into()
                .unwrap_or_default(),
        );
        if code == u32::MAX {
            return Err(CodecError::Corrupt("lzma range coder start"));
        }
        Ok(Self {
            b,
            pos: 5,
            range: u32::MAX,
            code,
        })
    }

    fn normalize(&mut self) -> Result<()> {
        if self.range < 1 << 24 {
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(byte(self.b, self.pos)?);
            self.pos += 1;
        }
        Ok(())
    }

    fn bit(&mut self, p: &mut u16) -> Result<usize> {
        let bound = (self.range >> 11) * u32::from(*p);
        let bit = if self.code < bound {
            self.range = bound;
            *p += (2048 - *p) >> 5;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *p -= *p >> 5;
            1
        };
        self.normalize()?;
        Ok(bit)
    }

    fn direct(&mut self, n: u32) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            self.range >>= 1;
            let bit = u32::from(self.code >= self.range);
            if bit == 1 {
                self.code -= self.range;
            }
            v = (v << 1) | bit;
            self.normalize()?;
        }
        Ok(v)
    }

    fn prob(&mut self, probs: &mut [u16], i: usize) -> Result<usize> {
        self.bit(
            probs
                .get_mut(i)
                .ok_or(CodecError::Corrupt("lzma probability index"))?,
        )
    }

    /// A bit-tree symbol of `bits` bits, most significant bit first.
    fn tree(&mut self, probs: &mut [u16], bits: u32) -> Result<usize> {
        let mut m = 1usize;
        for _ in 0..bits {
            m = (m << 1) | self.prob(probs, m)?;
        }
        Ok(m - (1 << bits))
    }

    /// A bit-tree symbol read least significant bit first.
    fn reverse(&mut self, probs: &mut [u16], bits: u32) -> Result<usize> {
        let (mut m, mut sym) = (1usize, 0usize);
        for i in 0..bits {
            let b = self.prob(probs, m)?;
            m = (m << 1) | b;
            sym |= b << i;
        }
        Ok(sym)
    }
}

const INIT: u16 = 1024;
const STATES: usize = 12;
const POS_STATES: usize = 16;

struct LenCoder {
    choice: [u16; 2],
    low: Vec<u16>,
    mid: Vec<u16>,
    high: Vec<u16>,
}

impl LenCoder {
    fn new() -> Self {
        Self {
            choice: [INIT; 2],
            low: vec![INIT; POS_STATES << 3],
            mid: vec![INIT; POS_STATES << 3],
            high: vec![INIT; 256],
        }
    }

    fn decode(&mut self, rc: &mut Rc<'_>, pos_state: usize) -> Result<usize> {
        let [c0, c1] = &mut self.choice;
        let range = pos_state << 3..(pos_state + 1) << 3;
        if rc.bit(c0)? == 0 {
            let t = self
                .low
                .get_mut(range)
                .ok_or(CodecError::Corrupt("lzma length"))?;
            return rc.tree(t, 3);
        }
        if rc.bit(c1)? == 0 {
            let t = self
                .mid
                .get_mut(range)
                .ok_or(CodecError::Corrupt("lzma length"))?;
            return Ok(8 + rc.tree(t, 3)?);
        }
        Ok(16 + rc.tree(&mut self.high, 8)?)
    }
}

/// LZMA decoder state that survives between LZMA2 chunks until a state reset.
struct Lzma {
    lc: u32,
    lp: u32,
    pb: u32,
    literal: Vec<u16>,
    is_match: Vec<u16>,
    is_rep: [u16; STATES],
    is_g0: [u16; STATES],
    is_g1: [u16; STATES],
    is_g2: [u16; STATES],
    is_rep0_long: Vec<u16>,
    slot: Vec<u16>,
    special: Vec<u16>,
    align: Vec<u16>,
    len: LenCoder,
    rep_len: LenCoder,
    state: usize,
    reps: [usize; 4],
}

impl Lzma {
    fn new(props: u8) -> Result<Self> {
        let d = u32::from(props);
        let (lc, lp, pb) = (d % 9, (d / 9) % 5, d / 45);
        if pb > 4 || lc + lp > 4 {
            return Err(CodecError::Corrupt("lzma2 properties"));
        }
        Ok(Self {
            lc,
            lp,
            pb,
            literal: vec![INIT; 0x300 << (lc + lp)],
            is_match: vec![INIT; STATES * POS_STATES],
            is_rep: [INIT; STATES],
            is_g0: [INIT; STATES],
            is_g1: [INIT; STATES],
            is_g2: [INIT; STATES],
            is_rep0_long: vec![INIT; STATES * POS_STATES],
            slot: vec![INIT; 4 << 6],
            special: vec![INIT; 115],
            align: vec![INIT; 16],
            len: LenCoder::new(),
            rep_len: LenCoder::new(),
            state: 0,
            reps: [0; 4],
        })
    }

    fn reset(&mut self) -> Result<()> {
        let props = self.lc + 9 * (self.lp + 5 * self.pb);
        *self = Self::new(u8::try_from(props).unwrap_or(u8::MAX))?;
        Ok(())
    }

    fn distance(&mut self, rc: &mut Rc<'_>, len: usize) -> Result<usize> {
        let lstate = len.min(3);
        let slots = self
            .slot
            .get_mut(lstate << 6..(lstate + 1) << 6)
            .ok_or(CodecError::Corrupt("lzma slot"))?;
        let slot = rc.tree(slots, 6)?;
        if slot < 4 {
            return Ok(slot);
        }
        let direct = u32::try_from((slot >> 1) - 1).unwrap_or(0);
        let base = (2 | (slot & 1)) << direct;
        if slot < 14 {
            let probs = self
                .special
                .get_mut(base - slot..)
                .ok_or(CodecError::Corrupt("lzma distance"))?;
            return Ok(base + rc.reverse(probs, direct)?);
        }
        let high = rc.direct(direct - 4)? as usize;
        Ok(base + (high << 4) + rc.reverse(&mut self.align, 4)?)
    }

    /// Decodes one chunk: appends until `out` reaches `end`. Matches reach back no further than
    /// `dict_start`, the output position of the last dictionary reset.
    fn chunk(&mut self, rc: &mut Rc<'_>, out: &mut Vec<u8>, dict_start: usize, end: usize) -> Result<()> {
        let lit_mask = (1usize << self.lp) - 1;
        let pos_mask = (1usize << self.pb) - 1;
        while out.len() < end {
            let pos = out.len() - dict_start;
            let ps = pos & pos_mask;
            let st = self.state;
            if rc.prob(&mut self.is_match, (st << 4) + ps)? == 0 {
                let prev = if pos > 0 {
                    out.last().copied().unwrap_or(0)
                } else {
                    0
                };
                let ls = ((pos & lit_mask) << self.lc) + (usize::from(prev) >> (8 - self.lc));
                let probs = self
                    .literal
                    .get_mut(0x300 * ls..0x300 * (ls + 1))
                    .ok_or(CodecError::Corrupt("lzma literal"))?;
                let mut sym = 1usize;
                if st >= 7 {
                    let mut mb = usize::from(back(out, dict_start, self.reps[0] + 1)?);
                    while sym < 0x100 {
                        let mbit = (mb >> 7) & 1;
                        mb <<= 1;
                        let bit = rc.prob(probs, ((1 + mbit) << 8) + sym)?;
                        sym = (sym << 1) | bit;
                        if mbit != bit {
                            break;
                        }
                    }
                }
                while sym < 0x100 {
                    sym = (sym << 1) | rc.prob(probs, sym)?;
                }
                out.push(u8::try_from(sym - 0x100).unwrap_or(0));
                self.state = if st < 4 {
                    0
                } else if st < 10 {
                    st - 3
                } else {
                    st - 6
                };
                continue;
            }
            let len;
            if rc.bit(self.is_rep.get_mut(st).ok_or(CodecError::Corrupt("lzma state"))?)? == 1 {
                if pos == 0 {
                    return Err(CodecError::Corrupt("lzma repeat before any output"));
                }
                if rc.bit(self.is_g0.get_mut(st).ok_or(CodecError::Corrupt("lzma state"))?)? == 0 {
                    if rc.prob(&mut self.is_rep0_long, (st << 4) + ps)? == 0 {
                        self.state = if st < 7 { 9 } else { 11 };
                        out.push(back(out, dict_start, self.reps[0] + 1)?);
                        continue;
                    }
                } else {
                    let [r0, r1, r2, r3] = self.reps;
                    let d = if rc.bit(self.is_g1.get_mut(st).ok_or(CodecError::Corrupt("lzma state"))?)? == 0
                    {
                        self.reps = [r1, r0, r2, r3];
                        r1
                    } else if rc.bit(self.is_g2.get_mut(st).ok_or(CodecError::Corrupt("lzma state"))?)? == 0 {
                        self.reps = [r2, r0, r1, r3];
                        r2
                    } else {
                        self.reps = [r3, r0, r1, r2];
                        r3
                    };
                    debug_assert_eq!(self.reps[0], d);
                }
                len = self.rep_len.decode(rc, ps)?;
                self.state = if st < 7 { 8 } else { 11 };
            } else {
                len = self.len.decode(rc, ps)?;
                self.state = if st < 7 { 7 } else { 10 };
                let d = self.distance(rc, len)?;
                if d == 0xFFFF_FFFF {
                    return Err(CodecError::Corrupt("lzma end marker inside lzma2"));
                }
                let [r0, r1, r2, _] = self.reps;
                self.reps = [d, r0, r1, r2];
            }
            let len = len + 2;
            let dist = self.reps[0] + 1;
            if dist > out.len() - dict_start {
                return Err(CodecError::Corrupt("lzma match before the dictionary"));
            }
            if out.len() + len > end {
                return Err(CodecError::Corrupt("lzma match past the chunk"));
            }
            crate::copy_match(out, dist, len, end)?;
        }
        Ok(())
    }
}

/// The byte `dist` positions back, within the current dictionary.
fn back(out: &[u8], dict_start: usize, dist: usize) -> Result<u8> {
    if dist > out.len() - dict_start {
        return Err(CodecError::Corrupt("lzma match before the dictionary"));
    }
    out.get(out.len() - dist)
        .copied()
        .ok_or(CodecError::Corrupt("lzma match"))
}

/// Decodes one LZMA2 stream from the start of `b`; returns the bytes it took.
fn lzma2(b: &[u8], out: &mut Vec<u8>, out_len: usize) -> Result<usize> {
    let mut at = 0usize;
    let mut dict_start = out.len();
    let mut need_dict = true;
    let mut lzma: Option<Lzma> = None;
    loop {
        let control = byte(b, at)?;
        match control {
            0 => return Ok(at + 1),
            1 | 2 => {
                if control == 1 {
                    dict_start = out.len();
                    need_dict = false;
                } else if need_dict {
                    return Err(CodecError::Corrupt("lzma2 without a dictionary reset"));
                }
                let n = be16(b, at + 1)? + 1;
                if out.len() + n > out_len {
                    return Err(CodecError::Corrupt("xz output larger than expected"));
                }
                out.extend_from_slice(b.get(at + 3..at + 3 + n).ok_or(CodecError::Truncated)?);
                at += 3 + n;
            }
            3..=0x7f => return Err(CodecError::Corrupt("lzma2 control byte")),
            _ => {
                let unpacked = ((usize::from(control & 0x1f) << 16) | be16(b, at + 1)?) + 1;
                let packed = be16(b, at + 3)? + 1;
                at += 5;
                let reset = (control >> 5) & 3;
                if reset == 3 {
                    dict_start = out.len();
                    need_dict = false;
                } else if need_dict {
                    return Err(CodecError::Corrupt("lzma2 without a dictionary reset"));
                }
                if reset >= 2 {
                    lzma = Some(Lzma::new(byte(b, at)?)?);
                    at += 1;
                }
                let l = lzma
                    .as_mut()
                    .ok_or(CodecError::Corrupt("lzma2 without properties"))?;
                if reset == 1 {
                    l.reset()?;
                }
                let end = out.len() + unpacked;
                if end > out_len {
                    return Err(CodecError::Corrupt("xz output larger than expected"));
                }
                let mut rc = Rc::new(b.get(at..at + packed).ok_or(CodecError::Truncated)?)?;
                l.chunk(&mut rc, out, dict_start, end)?;
                at += packed;
            }
        }
    }
}

fn check_size(kind: u8) -> usize {
    match kind {
        0 => 0,
        1..=3 => 4,
        4..=6 => 8,
        7..=9 => 16,
        10..=12 => 32,
        _ => 64,
    }
}

/// One stream: header, blocks, index, footer. Returns where it ends.
fn stream(input: &[u8], start: usize, out: &mut Vec<u8>, out_len: usize) -> Result<usize> {
    let s = input.get(start..).ok_or(CodecError::Truncated)?;
    if s.get(..6) != Some(MAGIC) {
        return Err(CodecError::Corrupt("not an xz stream"));
    }
    let flags = s.get(6..8).ok_or(CodecError::Truncated)?;
    if byte(flags, 0)? != 0 || byte(flags, 1)? & 0xf0 != 0 || crc32(flags) != le32(s, 8)? {
        return Err(CodecError::Corrupt("xz stream flags"));
    }
    let check = byte(flags, 1)?;
    let mut at = 12usize;
    let mut records: Vec<(u64, u64)> = Vec::new();
    loop {
        let size_byte = byte(s, at)?;
        if size_byte == 0 {
            break;
        }
        let hsize = (usize::from(size_byte) + 1) * 4;
        let h = s.get(at..at + hsize).ok_or(CodecError::Truncated)?;
        if crc32(h.get(..hsize - 4).unwrap_or_default()) != le32(h, hsize - 4)? {
            return Err(CodecError::Corrupt("xz block header CRC"));
        }
        let bflags = byte(h, 1)?;
        if bflags & 0x3c != 0 || bflags & 3 != 0 {
            return Err(CodecError::Corrupt(
                "xz block flags (only a single LZMA2 filter is supported)",
            ));
        }
        let mut p = 2usize;
        let compressed = if bflags & 0x40 != 0 {
            Some(varint(h, &mut p)?)
        } else {
            None
        };
        let uncompressed = if bflags & 0x80 != 0 {
            Some(varint(h, &mut p)?)
        } else {
            None
        };
        if varint(h, &mut p)? != LZMA2_FILTER || varint(h, &mut p)? != 1 || byte(h, p)? > 40 {
            return Err(CodecError::Corrupt("xz filter (only LZMA2 is supported)"));
        }
        p += 1;
        if p > hsize - 4 || h.get(p..hsize - 4).is_some_and(|pad| pad.iter().any(|&x| x != 0)) {
            return Err(CodecError::Corrupt("xz block header"));
        }
        let data_start = at + hsize;
        let first = out.len();
        let used = lzma2(s.get(data_start..).unwrap_or_default(), out, out_len)?;
        let produced = (out.len() - first) as u64;
        if compressed.is_some_and(|c| c != used as u64) || uncompressed.is_some_and(|u| u != produced) {
            return Err(CodecError::Corrupt("xz block sizes"));
        }
        at = data_start + used;
        while !at.is_multiple_of(4) {
            if byte(s, at)? != 0 {
                return Err(CodecError::Corrupt("xz block padding"));
            }
            at += 1;
        }
        let n = check_size(check);
        let want = s.get(at..at + n).ok_or(CodecError::Truncated)?;
        let data = out.get(first..).unwrap_or_default();
        let ok = match check {
            1 => crc32(data).to_le_bytes() == want,
            4 => crc64(data).to_le_bytes() == want,
            _ => true,
        };
        if !ok {
            return Err(CodecError::Corrupt("xz block check"));
        }
        at += n;
        records.push(((hsize + used + n) as u64, produced));
    }
    // Index.
    let index_start = at;
    at += 1;
    if varint(s, &mut at)? != records.len() as u64 {
        return Err(CodecError::Corrupt("xz index count"));
    }
    for &(unpadded, size) in &records {
        if varint(s, &mut at)? != unpadded || varint(s, &mut at)? != size {
            return Err(CodecError::Corrupt("xz index record"));
        }
    }
    while !at.is_multiple_of(4) {
        if byte(s, at)? != 0 {
            return Err(CodecError::Corrupt("xz index padding"));
        }
        at += 1;
    }
    if crc32(s.get(index_start..at).unwrap_or_default()) != le32(s, at)? {
        return Err(CodecError::Corrupt("xz index CRC"));
    }
    at += 4;
    // Footer.
    let footer = s.get(at..at + 12).ok_or(CodecError::Truncated)?;
    let backward = (u64::from(le32(footer, 4)?) + 1) * 4;
    if crc32(footer.get(4..10).unwrap_or_default()) != le32(footer, 0)?
        || footer.get(8..10) != Some(flags)
        || footer.get(10..12) != Some(FOOTER_MAGIC)
        || backward != (at - index_start) as u64
    {
        return Err(CodecError::Corrupt("xz stream footer"));
    }
    Ok(start + at + 12)
}

/// Decompresses an xz file (one or more streams, with stream padding). The output may not
/// exceed `out_len` bytes.
pub fn decompress(input: &[u8], out_len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(out_len.min(1 << 20));
    let mut at = stream(input, 0, &mut out, out_len)?;
    loop {
        while input.get(at..at + 4) == Some(&[0; 4]) {
            at += 4;
        }
        if at >= input.len() {
            return Ok(out);
        }
        at = stream(input, at, &mut out, out_len)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_the_xz_specification() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc64(b"123456789"), 0x995D_C9BB_DF19_39FA);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decompress(b"", 10).is_err());
        assert!(decompress(b"\xFD7zXZ\0\0\x01", 10).is_err());
    }
}
