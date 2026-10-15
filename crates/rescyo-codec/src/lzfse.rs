//! LZFSE and LZVN (DMG ULFO chunks, APFS compressed files), written from the format Apple's
//! open-source reference produces.
//!
//! A stream is a sequence of blocks, each starting with a magic: `bvx-` (stored), `bvxn` (LZVN),
//! `bvx1`/`bvx2` (LZFSE with plain or packed header) and `bvx$` (end). An LZFSE block holds
//! FSE-coded literals and FSE-coded (literal length, match length, distance) triples, both read
//! backwards from the end of their payload. Matches may reach into earlier blocks.

use crate::{CodecError, Result, le16, le32};

const END: u32 = u32::from_le_bytes(*b"bvx$");
const RAW: u32 = u32::from_le_bytes(*b"bvx-");
const V1: u32 = u32::from_le_bytes(*b"bvx1");
const V2: u32 = u32::from_le_bytes(*b"bvx2");
const VN: u32 = u32::from_le_bytes(*b"bvxn");

const L_SYMBOLS: usize = 20;
const M_SYMBOLS: usize = 20;
const D_SYMBOLS: usize = 64;
const LIT_SYMBOLS: usize = 256;
const L_STATES: usize = 64;
const M_STATES: usize = 64;
const D_STATES: usize = 256;
const LIT_STATES: usize = 1024;
const MATCHES_PER_BLOCK: usize = 10_000;
const LITERALS_PER_BLOCK: usize = 4 * MATCHES_PER_BLOCK;
/// `sizeof(lzfse_compressed_block_header_v1)`: 770 bytes of fields padded to 4.
const V1_HEADER: usize = 772;
const V2_FIXED: usize = 32;

const L_EXTRA: [u8; L_SYMBOLS] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 5, 8];
const M_EXTRA: [u8; M_SYMBOLS] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 5, 8, 11];

/// Distance symbol `i` carries `i / 4` extra bits (none for the first four).
fn d_extra(i: usize) -> u8 {
    u8::try_from(i / 4).unwrap_or(0)
}

/// Base values follow from the extra-bit counts: each symbol starts where the previous ends.
fn bases(extra: impl Fn(usize) -> u8, n: usize) -> Vec<u32> {
    let mut v = Vec::with_capacity(n);
    let mut b = 0u32;
    for i in 0..n {
        v.push(b);
        b += 1 << extra(i);
    }
    v
}

/// The decoded fields of a compressed block header (v2 headers unpack into the same shape).
struct Header {
    n_raw: usize,
    n_literals: usize,
    n_matches: usize,
    lit_payload: usize,
    lmd_payload: usize,
    literal_bits: i32,
    literal_state: [u16; 4],
    lmd_bits: i32,
    l_state: u16,
    m_state: u16,
    d_state: u16,
    /// l, m, d and literal frequencies, in that order.
    freq: Vec<u16>,
}

const FREQ_COUNT: usize = L_SYMBOLS + M_SYMBOLS + D_SYMBOLS + LIT_SYMBOLS;

fn field(v: u64, at: u32, bits: u32) -> u64 {
    (v >> at) & ((1u64 << bits) - 1)
}

fn wide(v: u64) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// One frequency of a v2 header: a prefix code over the low bits of `bits`.
fn freq_value(bits: u32) -> (u16, u32) {
    const NBITS: [u8; 32] = [
        2, 3, 2, 5, 2, 3, 2, 8, 2, 3, 2, 5, 2, 3, 2, 14, 2, 3, 2, 5, 2, 3, 2, 8, 2, 3, 2, 5, 2, 3, 2, 14,
    ];
    const VALUE: [u8; 32] = [
        0, 2, 1, 4, 0, 3, 1, 0, 0, 2, 1, 5, 0, 3, 1, 0, 0, 2, 1, 6, 0, 3, 1, 0, 0, 2, 1, 7, 0, 3, 1, 0,
    ];
    let b = (bits & 31) as usize;
    let n = u32::from(NBITS.get(b).copied().unwrap_or(2));
    let v = match n {
        8 => 8 + ((bits >> 4) & 0xf),
        14 => 24 + ((bits >> 4) & 0x3ff),
        _ => u32::from(VALUE.get(b).copied().unwrap_or(0)),
    };
    (u16::try_from(v).unwrap_or(0), n)
}

fn header_v2(b: &[u8]) -> Result<(Header, usize)> {
    let n_raw = le32(b, 4).ok_or(CodecError::Truncated)? as usize;
    let f0 = le64(b, 8).ok_or(CodecError::Truncated)?;
    let f1 = le64(b, 16).ok_or(CodecError::Truncated)?;
    let f2 = le64(b, 24).ok_or(CodecError::Truncated)?;
    let size = wide(field(f2, 0, 32));
    if !(V2_FIXED..=V2_FIXED + FREQ_COUNT * 2).contains(&size) {
        return Err(CodecError::Corrupt("lzfse header size"));
    }
    let packed = b.get(V2_FIXED..size).ok_or(CodecError::Truncated)?;
    let mut freq = Vec::with_capacity(FREQ_COUNT);
    let (mut acc, mut acc_bits, mut src) = (0u32, 0u32, packed.iter());
    for _ in 0..FREQ_COUNT {
        while acc_bits + 8 <= 32 {
            let Some(&byte) = src.next() else { break };
            acc |= u32::from(byte) << acc_bits;
            acc_bits += 8;
        }
        let (v, n) = freq_value(acc);
        if n > acc_bits {
            return Err(CodecError::Corrupt("lzfse frequency table"));
        }
        freq.push(v);
        acc >>= n;
        acc_bits -= n;
    }
    if acc_bits >= 8 || src.next().is_some() {
        return Err(CodecError::Corrupt("lzfse frequency table length"));
    }
    let small = |v: u64| u16::try_from(v).unwrap_or(u16::MAX);
    let h = Header {
        n_raw,
        n_literals: wide(field(f0, 0, 20)),
        lit_payload: wide(field(f0, 20, 20)),
        n_matches: wide(field(f0, 40, 20)),
        literal_bits: i32::from(small(field(f0, 60, 3))) - 7,
        literal_state: [
            small(field(f1, 0, 10)),
            small(field(f1, 10, 10)),
            small(field(f1, 20, 10)),
            small(field(f1, 30, 10)),
        ],
        lmd_payload: wide(field(f1, 40, 20)),
        lmd_bits: i32::from(small(field(f1, 60, 3))) - 7,
        l_state: small(field(f2, 32, 10)),
        m_state: small(field(f2, 42, 10)),
        d_state: small(field(f2, 52, 10)),
        freq,
    };
    Ok((h, size))
}

fn header_v1(b: &[u8]) -> Result<(Header, usize)> {
    let u32_at = |at| le32(b, at).ok_or(CodecError::Truncated);
    let u16_at = |at| le16(b, at).ok_or(CodecError::Truncated);
    let i32_at = |at| {
        le32(b, at)
            .map(|v| i32::from_le_bytes(v.to_le_bytes()))
            .ok_or(CodecError::Truncated)
    };
    let mut freq = Vec::with_capacity(FREQ_COUNT);
    for i in 0..FREQ_COUNT {
        freq.push(u16_at(50 + 2 * i)?);
    }
    if b.len() < V1_HEADER {
        return Err(CodecError::Truncated);
    }
    let h = Header {
        n_raw: u32_at(4)? as usize,
        n_literals: u32_at(12)? as usize,
        n_matches: u32_at(16)? as usize,
        lit_payload: u32_at(20)? as usize,
        lmd_payload: u32_at(24)? as usize,
        literal_bits: i32_at(28)?,
        literal_state: [u16_at(32)?, u16_at(34)?, u16_at(36)?, u16_at(38)?],
        lmd_bits: i32_at(40)?,
        l_state: u16_at(44)?,
        m_state: u16_at(46)?,
        d_state: u16_at(48)?,
        freq,
    };
    Ok((h, V1_HEADER))
}

/// Bits read backwards from the end of a payload, most significant first, as the encoder
/// flushed them.
struct BackBits<'a> {
    buf: &'a [u8],
    /// Bytes before this index are still unread.
    pos: usize,
    acc: u64,
    nbits: u32,
}

impl<'a> BackBits<'a> {
    fn new(buf: &'a [u8], extra: i32) -> Result<Self> {
        let take = if extra == 0 { 7 } else { 8 };
        let pos = buf.len().checked_sub(take).ok_or(CodecError::Truncated)?;
        let mut acc = 0u64;
        for (k, &byte) in buf.get(pos..).unwrap_or_default().iter().enumerate() {
            acc |= u64::from(byte) << (8 * k);
        }
        let nbits = i32::try_from(take * 8).unwrap_or(64) + extra;
        let nbits = u32::try_from(nbits).map_err(|_| CodecError::Corrupt("lzfse bit count"))?;
        if !(56..64).contains(&nbits) || acc >> nbits != 0 {
            return Err(CodecError::Corrupt("lzfse bit stream start"));
        }
        Ok(Self { buf, pos, acc, nbits })
    }

    /// Tops the accumulator up to at least 56 bits.
    fn refill(&mut self) -> Result<()> {
        let n = (63 - self.nbits) & !7;
        let bytes = (n / 8) as usize;
        if bytes == 0 {
            return Ok(());
        }
        let start = self.pos.checked_sub(bytes).ok_or(CodecError::Truncated)?;
        let mut incoming = 0u64;
        for (k, &byte) in self
            .buf
            .get(start..self.pos)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            incoming |= u64::from(byte) << (8 * k);
        }
        self.acc = (self.acc << n) | incoming;
        self.nbits += n;
        self.pos = start;
        Ok(())
    }

    fn pull(&mut self, n: u32) -> Result<u64> {
        if n > self.nbits {
            return Err(CodecError::Corrupt("lzfse bit stream underflow"));
        }
        self.nbits -= n;
        let v = self.acc >> self.nbits;
        self.acc &= (1u64 << self.nbits) - 1;
        Ok(v)
    }
}

#[derive(Clone, Copy, Default)]
struct Entry {
    /// Bits to pull: the state's share plus, for value tables, the value's extra bits.
    bits: u8,
    extra: u8,
    delta: u16,
    symbol: u8,
    base: u32,
}

/// Builds an FSE decoding table: each symbol owns `freq` consecutive states, and a state tells
/// how many bits to read and which state range they index into.
fn fse_table(nstates: usize, freq: &[u16], extra: &dyn Fn(usize) -> u8, base: &[u32]) -> Result<Vec<Entry>> {
    let mut t = Vec::with_capacity(nstates);
    let n_lz = nstates.leading_zeros();
    for (sym, &f) in freq.iter().enumerate() {
        let f = usize::from(f);
        if f == 0 {
            continue;
        }
        if t.len() + f > nstates {
            return Err(CodecError::Corrupt("lzfse frequencies exceed the state count"));
        }
        let k = f.leading_zeros() - n_lz;
        let j0 = ((2 * nstates) >> k) - f;
        let e0 = Entry {
            extra: extra(sym),
            symbol: u8::try_from(sym).unwrap_or(0),
            base: base.get(sym).copied().unwrap_or(0),
            ..Entry::default()
        };
        for j in 0..f {
            let (kb, delta) = if j < j0 {
                (k, ((f + j) << k) - nstates)
            } else {
                (k - 1, (j - j0) << (k - 1))
            };
            t.push(Entry {
                bits: u8::try_from(kb).unwrap_or(0) + e0.extra,
                delta: u16::try_from(delta).map_err(|_| CodecError::Corrupt("lzfse table"))?,
                ..e0
            });
        }
    }
    // States no symbol owns are invalid; a stream that reaches one is corrupt.
    t.resize(
        nstates,
        Entry {
            bits: u8::MAX,
            ..Entry::default()
        },
    );
    Ok(t)
}

/// Decodes one value (or literal) and advances the state.
fn step(t: &[Entry], state: &mut usize, bits: &mut BackBits<'_>) -> Result<(u8, u32)> {
    let e = *t.get(*state).ok_or(CodecError::Corrupt("lzfse state"))?;
    if e.bits == u8::MAX {
        return Err(CodecError::Corrupt("lzfse state"));
    }
    let v = bits.pull(u32::from(e.bits))?;
    let value_bits = v & ((1u64 << e.extra) - 1);
    *state = usize::from(e.delta) + usize::try_from(v >> e.extra).unwrap_or(usize::MAX);
    if *state >= t.len() {
        return Err(CodecError::Corrupt("lzfse state"));
    }
    Ok((e.symbol, e.base + u32::try_from(value_bits).unwrap_or(0)))
}

/// `block` runs from the block's magic to the end of its payload. Like the reference decoder,
/// the backwards bit readers may start inside the bytes before their own payload (a stream with
/// no literals has an empty literal payload), so they are given everything up to their end.
fn lzfse_block(h: &Header, block: &[u8], size: usize, out: &mut Vec<u8>) -> Result<()> {
    if h.n_literals > LITERALS_PER_BLOCK || h.n_matches > MATCHES_PER_BLOCK {
        return Err(CodecError::Corrupt("lzfse block counts"));
    }
    let (lf, rest) = h.freq.split_at(L_SYMBOLS);
    let (mf, rest) = rest.split_at(M_SYMBOLS);
    let (df, litf) = rest.split_at(D_SYMBOLS);
    let none = |_: usize| 0u8;
    let lit_t = fse_table(LIT_STATES, litf, &none, &[])?;
    let l_t = fse_table(
        L_STATES,
        lf,
        &|i| L_EXTRA.get(i).copied().unwrap_or(0),
        &bases(|i| L_EXTRA.get(i).copied().unwrap_or(0), L_SYMBOLS),
    )?;
    let m_t = fse_table(
        M_STATES,
        mf,
        &|i| M_EXTRA.get(i).copied().unwrap_or(0),
        &bases(|i| M_EXTRA.get(i).copied().unwrap_or(0), M_SYMBOLS),
    )?;
    let d_t = fse_table(D_STATES, df, &d_extra, &bases(d_extra, D_SYMBOLS))?;

    // Literals: four interleaved FSE states over one backwards bit stream.
    let lit_buf = block.get(..size + h.lit_payload).ok_or(CodecError::Truncated)?;
    let mut bits = BackBits::new(lit_buf, h.literal_bits)?;
    let mut states = h.literal_state.map(usize::from);
    let mut literals = Vec::with_capacity(h.n_literals + 4);
    while literals.len() < h.n_literals {
        bits.refill()?;
        for s in &mut states {
            literals.push(step(&lit_t, s, &mut bits)?.0);
        }
    }

    // Literal length, match length, distance triples.
    let lmd_buf = block
        .get(..size + h.lit_payload + h.lmd_payload)
        .ok_or(CodecError::Truncated)?;
    let mut bits = BackBits::new(lmd_buf, h.lmd_bits)?;
    let (mut ls, mut ms, mut ds) = (
        usize::from(h.l_state),
        usize::from(h.m_state),
        usize::from(h.d_state),
    );
    let start = out.len();
    let limit = start + h.n_raw;
    let (mut lit_pos, mut dist) = (0usize, 0usize);
    for _ in 0..h.n_matches {
        bits.refill()?;
        let l = step(&l_t, &mut ls, &mut bits)?.1 as usize;
        let m = step(&m_t, &mut ms, &mut bits)?.1 as usize;
        let d = step(&d_t, &mut ds, &mut bits)?.1 as usize;
        if d != 0 {
            dist = d;
        }
        let lits = literals
            .get(lit_pos..lit_pos + l)
            .ok_or(CodecError::Corrupt("lzfse literal run"))?;
        if out.len() + l + m > limit {
            return Err(CodecError::Corrupt("lzfse block larger than declared"));
        }
        out.extend_from_slice(lits);
        lit_pos += l;
        if m > 0 {
            crate::copy_match(out, dist, m, limit)?;
        }
    }
    if out.len() != limit {
        return Err(CodecError::Corrupt("lzfse block size"));
    }
    Ok(())
}

/// Decodes an LZVN payload, appending exactly `n_raw` bytes to `out`.
fn lzvn_block(src: &[u8], n_raw: usize, out: &mut Vec<u8>) -> Result<()> {
    let limit = out.len() + n_raw;
    let byte = |at: usize| src.get(at).copied().map(usize::from).ok_or(CodecError::Truncated);
    let (mut i, mut dist) = (0usize, 0usize);
    loop {
        let op = byte(i)?;
        // (instruction length, literal count, match length, new distance)
        let (len, lit, m, d) = match op {
            0x06 => break,
            0x0e | 0x16 => (1, 0, 0, None),
            0x70..=0x7f => return Err(CodecError::Corrupt("lzvn undefined opcode")),
            0xa0..=0xbf => {
                let b1 = byte(i + 1)?;
                let b2 = byte(i + 2)?;
                let m = (((op & 7) << 2) | (b1 & 3)) + 3;
                (3, (op >> 3) & 3, m, Some((b2 << 6) | (b1 >> 2)))
            }
            0xe0 => (2, byte(i + 1)? + 16, 0, None),
            0xe1..=0xef => (1, op & 0xf, 0, None),
            0xf0 => (2, 0, byte(i + 1)? + 16, None),
            0xf1..=0xff => (1, 0, op & 0xf, None),
            _ => match op & 7 {
                7 => (
                    3,
                    op >> 6,
                    ((op >> 3) & 7) + 3,
                    Some(byte(i + 1)? | (byte(i + 2)? << 8)),
                ),
                6 if op >= 0x40 => (1, op >> 6, ((op >> 3) & 7) + 3, None),
                6 => return Err(CodecError::Corrupt("lzvn undefined opcode")),
                _ => (
                    2,
                    op >> 6,
                    ((op >> 3) & 7) + 3,
                    Some(((op & 7) << 8) | byte(i + 1)?),
                ),
            },
        };
        let lits = src.get(i + len..i + len + lit).ok_or(CodecError::Truncated)?;
        if out.len() + lit + m > limit {
            return Err(CodecError::Corrupt("lzvn block larger than declared"));
        }
        out.extend_from_slice(lits);
        i += len + lit;
        if let Some(d) = d {
            dist = d;
        }
        if m > 0 {
            crate::copy_match(out, dist, m, limit)?;
        }
    }
    if out.len() != limit {
        return Err(CodecError::Corrupt("lzvn block size"));
    }
    Ok(())
}

/// Decompresses a whole LZFSE stream. The output may not exceed `out_len` bytes.
pub fn decompress(input: &[u8], out_len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(out_len.min(1 << 20));
    let mut at = 0usize;
    loop {
        let block = input.get(at..).ok_or(CodecError::Truncated)?;
        let magic = le32(block, 0).ok_or(CodecError::Truncated)?;
        let n_raw = || le32(block, 4).map(|v| v as usize).ok_or(CodecError::Truncated);
        let too_big = |n: usize| out.len().saturating_add(n) > out_len;
        match magic {
            END => return Ok(out),
            RAW => {
                let n = n_raw()?;
                if too_big(n) {
                    return Err(CodecError::Corrupt("lzfse output larger than expected"));
                }
                out.extend_from_slice(block.get(8..8 + n).ok_or(CodecError::Truncated)?);
                at += 8 + n;
            }
            VN => {
                let n = n_raw()?;
                let payload = le32(block, 8).ok_or(CodecError::Truncated)? as usize;
                if too_big(n) {
                    return Err(CodecError::Corrupt("lzfse output larger than expected"));
                }
                lzvn_block(
                    block.get(12..12 + payload).ok_or(CodecError::Truncated)?,
                    n,
                    &mut out,
                )?;
                at += 12 + payload;
            }
            V1 | V2 => {
                let (h, size) = if magic == V1 {
                    header_v1(block)?
                } else {
                    header_v2(block)?
                };
                if too_big(h.n_raw) {
                    return Err(CodecError::Corrupt("lzfse output larger than expected"));
                }
                let total = h.lit_payload + h.lmd_payload;
                lzfse_block(
                    &h,
                    block.get(..size + total).ok_or(CodecError::Truncated)?,
                    size,
                    &mut out,
                )?;
                at += size + total;
            }
            _ => return Err(CodecError::Corrupt("lzfse block magic")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_and_lzvn_blocks() {
        let mut s = b"bvx-\x03\0\0\0abc".to_vec();
        // LZVN: three literals "xyz", a small-distance match (6 back, length 3), end of stream.
        s.extend_from_slice(b"bvxn\x06\0\0\0\x0e\0\0\0");
        s.extend_from_slice(&[0xe3, b'x', b'y', b'z', 0x00, 0x06, 0x06, 0, 0, 0, 0, 0, 0, 0]);
        s.extend_from_slice(b"bvx$");
        assert_eq!(decompress(&s, 64).unwrap(), b"abcxyzabc".to_vec());
        assert!(decompress(&s, 8).is_err(), "bounded by the caller");
        assert!(decompress(b"bvxn\x05\0\0\0\x01\0\0\0\x70bvx$", 64).is_err());
    }
}
