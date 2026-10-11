//! WOF (Windows Overlay Filter) file compression, as written by `compact /exe:…`.
//!
//! The file's unnamed stream stays sparse with the original size; the content lives in the
//! `WofCompressedData` named stream: a table of chunk end offsets (one per chunk but the last,
//! 32-bit unless the original is 4 GiB or larger), then the chunks. A chunk whose stored size
//! equals its original size is stored raw.

use std::ops::Range;

use crate::{CodecError, Result, lzx, xpress};

/// Compression algorithm from the WOF reparse point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Xpress4k,
    Lzx,
    Xpress8k,
    Xpress16k,
}

impl Algorithm {
    /// From `FILE_PROVIDER_EXTERNAL_INFO_V1.Algorithm`.
    pub fn from_id(id: u32) -> Option<Self> {
        Some(match id {
            0 => Self::Xpress4k,
            1 => Self::Lzx,
            2 => Self::Xpress8k,
            3 => Self::Xpress16k,
            _ => return None,
        })
    }

    pub fn chunk_size(self) -> u64 {
        match self {
            Self::Xpress4k => 4096,
            Self::Xpress8k => 8192,
            Self::Xpress16k => 16384,
            Self::Lzx => 32768,
        }
    }
}

/// Where the chunks of a WOF stream are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub algorithm: Algorithm,
    /// Original (uncompressed) size.
    pub size: u64,
    pub chunks: u64,
    entry: u64,
}

impl Layout {
    pub fn new(algorithm: Algorithm, size: u64) -> Self {
        let chunks = size.div_ceil(algorithm.chunk_size());
        let entry = if size > u64::from(u32::MAX) { 8 } else { 4 };
        Self {
            algorithm,
            size,
            chunks,
            entry,
        }
    }

    /// Bytes of the chunk table at the start of the stream.
    pub fn table_len(&self) -> u64 {
        self.chunks.saturating_sub(1) * self.entry
    }

    /// The table bytes holding the start and end of chunk `i` (relative to the stream).
    pub fn entries_for(&self, i: u64) -> Range<u64> {
        let first = if i == 0 { 0 } else { (i - 1) * self.entry };
        let last = ((i + 1) * self.entry).min(self.table_len());
        first..last
    }

    /// Range of chunk `i` in the stream, given the table bytes from [`Self::entries_for`] and the
    /// stream's total length.
    pub fn chunk_range(&self, i: u64, entries: &[u8], stream_len: u64) -> Result<Range<u64>> {
        let entry = usize::try_from(self.entry).unwrap_or(4);
        let read = |b: &[u8]| -> u64 {
            let mut v = [0u8; 8];
            if let Some(d) = v.get_mut(..b.len()) {
                d.copy_from_slice(b);
            }
            u64::from_le_bytes(v)
        };
        let mut vals = entries.chunks_exact(entry).map(read);
        let start = if i == 0 {
            0
        } else {
            vals.next().ok_or(CodecError::Truncated)?
        };
        let end = if i + 1 == self.chunks {
            stream_len - self.table_len()
        } else {
            vals.next().ok_or(CodecError::Truncated)?
        };
        let base = self.table_len();
        if start > end || base + end > stream_len {
            return Err(CodecError::Corrupt("WOF chunk table"));
        }
        Ok(base + start..base + end)
    }

    /// Original size of chunk `i`.
    pub fn chunk_len(&self, i: u64) -> u64 {
        let c = self.algorithm.chunk_size();
        (self.size - (i * c).min(self.size)).min(c)
    }

    /// Decodes chunk `i` from its stored bytes.
    pub fn decode(&self, i: u64, stored: &[u8]) -> Result<Vec<u8>> {
        let len = usize::try_from(self.chunk_len(i)).map_err(|_| CodecError::Corrupt("chunk size"))?;
        if stored.len() == len {
            return Ok(stored.to_vec());
        }
        if stored.len() > len {
            return Err(CodecError::Corrupt("chunk larger than its original"));
        }
        match self.algorithm {
            Algorithm::Lzx => lzx::decompress(stored, len),
            _ => xpress::decompress(stored, len),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_table_arithmetic() {
        let l = Layout::new(Algorithm::Xpress4k, 10_000);
        assert_eq!((l.chunks, l.table_len()), (3, 8));
        assert_eq!(l.entries_for(0), 0..4);
        assert_eq!(l.entries_for(1), 0..8);
        assert_eq!(l.entries_for(2), 4..8);
        // Chunk ends at 100 and 300; the stream holds 8 + 400 bytes.
        let table = [100u32.to_le_bytes(), 300u32.to_le_bytes()].concat();
        assert_eq!(l.chunk_range(0, &table[0..4], 408).unwrap(), 8..108);
        assert_eq!(l.chunk_range(1, &table[0..8], 408).unwrap(), 108..308);
        assert_eq!(l.chunk_range(2, &table[4..8], 408).unwrap(), 308..408);
        assert_eq!(l.chunk_len(2), 10_000 - 8192);
        assert!(l.chunk_range(1, &[200, 0, 0, 0, 100, 0, 0, 0], 408).is_err());
        // Raw chunks pass through.
        assert_eq!(l.decode(2, &[7; 1808]).unwrap(), vec![7; 1808]);
        assert_eq!(
            Layout::new(Algorithm::Lzx, 5 << 30).table_len(),
            ((5u64 << 30) / 32768 - 1) * 8
        );
    }

    #[test]
    fn xpress_chunks_decode() {
        let data = b"chunk of a WOF-compressed system file ".repeat(120);
        let l = Layout::new(Algorithm::Xpress4k, data.len() as u64);
        let c0 = crate::xpress::tests::compress(&data[..4096]);
        assert_eq!(l.decode(0, &c0).unwrap(), data[..4096]);
    }
}
