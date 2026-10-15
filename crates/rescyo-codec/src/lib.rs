//! Decoders for the compression file systems and disk images apply.
//!
//! - [`lznt1`]: classic NTFS compression (the "compress contents" attribute).
//! - [`xpress`]: XPRESS Huffman, used by WOF (`compact /exe:xpress4k|8k|16k`).
//! - [`lzx`]: LZX as used by WOF and WIM (`compact /exe:lzx`).
//! - [`wof`]: the chunk table WOF puts in front of the compressed chunks.
//! - [`adc`], [`bzip2`], [`lzfse`] (with LZVN) and [`xz`] (LZMA2): the chunk codecs of Apple
//!   disk images (UDCO, UDBZ, ULFO, ULMO).
//!
//! Written from the public specifications ([MS-XCA] for LZNT1 and XPRESS Huffman, [MS-PATCH]
//! for LZX), the published bitstream formats for the Apple and xz codecs. Every input is hostile: output is bounded by the caller's expected size, every read
//! is bounds-checked, and a malformed stream is an error, never a panic.
#![forbid(unsafe_code)]

pub mod adc;
pub mod bzip2;
mod huffman;
pub mod lzfse;
pub mod lznt1;
pub mod lzx;
pub mod wof;
pub mod xpress;
pub mod xz;

/// Why a compressed stream could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// The stream ended before the expected output was produced.
    Truncated,
    /// The stream is not valid for its format; the text says where.
    Corrupt(&'static str),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => f.write_str("compressed data ends early"),
            Self::Corrupt(why) => write!(f, "corrupt compressed data: {why}"),
        }
    }
}

impl std::error::Error for CodecError {}

pub type Result<T> = std::result::Result<T, CodecError>;

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Copies `len` bytes from `dist` bytes back, byte by byte (the source may overlap the output).
fn copy_match(out: &mut Vec<u8>, dist: usize, len: usize, limit: usize) -> Result<()> {
    if dist == 0 || dist > out.len() {
        return Err(CodecError::Corrupt("match before the start of the output"));
    }
    let len = len.min(limit.saturating_sub(out.len()));
    let start = out.len() - dist;
    for k in 0..len {
        let b = *out
            .get(start + k)
            .ok_or(CodecError::Corrupt("match out of range"))?;
        out.push(b);
    }
    Ok(())
}
