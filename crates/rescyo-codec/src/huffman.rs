//! Canonical Huffman decoding shared by XPRESS Huffman and LZX.
//!
//! Codes are assigned canonically (shorter codes first, then by symbol). Decoding walks the code
//! one bit at a time against per-length counts, so no table can be overrun by a hostile set of
//! lengths; an over-subscribed set is rejected, an incomplete one fails only when an unused code
//! is met.

use crate::{CodecError, Result};

pub struct Huffman {
    max_len: u32,
    /// Number of codes of each length (index 0 unused).
    counts: Vec<u16>,
    /// Symbols ordered by (length, symbol).
    symbols: Vec<u16>,
}

impl Huffman {
    /// Builds a decoder from code lengths (0 = symbol unused).
    pub fn new(lens: &[u8], max_len: u32) -> Result<Self> {
        let mut counts = vec![0u16; max_len as usize + 1];
        for &l in lens {
            if u32::from(l) > max_len {
                return Err(CodecError::Corrupt("code length too long"));
            }
            if l > 0 {
                let c = counts
                    .get_mut(usize::from(l))
                    .ok_or(CodecError::Corrupt("code length"))?;
                *c += 1;
            }
        }
        // Over-subscribed sets cannot be decoded unambiguously.
        let mut left: i64 = 1;
        for len in 1..=max_len as usize {
            left = left * 2 - i64::from(*counts.get(len).unwrap_or(&0));
            if left < 0 {
                return Err(CodecError::Corrupt("over-subscribed Huffman code"));
            }
        }
        let mut symbols = Vec::with_capacity(lens.len());
        for len in 1..=max_len {
            for (s, &l) in lens.iter().enumerate() {
                if u32::from(l) == len {
                    symbols.push(u16::try_from(s).map_err(|_| CodecError::Corrupt("too many symbols"))?);
                }
            }
        }
        Ok(Self {
            max_len,
            counts,
            symbols,
        })
    }

    pub fn max_len(&self) -> u32 {
        self.max_len
    }

    /// Decodes one symbol from `bits`, whose top `max_len` bits (of a `max_len`-bit window) hold
    /// the next code. Returns the symbol and its code length.
    pub fn decode(&self, window: u32) -> Result<(u16, u32)> {
        let (mut code, mut first, mut index) = (0i64, 0i64, 0i64);
        for len in 1..=self.max_len {
            code |= i64::from((window >> (self.max_len - len)) & 1);
            let count = i64::from(*self.counts.get(len as usize).unwrap_or(&0));
            if code - first < count {
                let at = usize::try_from(index + code - first).map_err(|_| CodecError::Corrupt("code"))?;
                let s = *self.symbols.get(at).ok_or(CodecError::Corrupt("code"))?;
                return Ok((s, len));
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(CodecError::Corrupt("invalid Huffman code"))
    }
}
