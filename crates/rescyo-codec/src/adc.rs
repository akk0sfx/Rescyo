//! ADC (Apple Data Compression), the LZ77 variant of old DMG images (UDCO).
//!
//! Each item starts with a control byte: high bit set, a literal run of `(b & 0x7f) + 1` bytes;
//! next bit set, a three-byte match (length `(b & 0x3f) + 4`, 16-bit distance + 1); otherwise a
//! two-byte match (length `((b >> 2) & 0xf) + 3`, 10-bit distance + 1).

use crate::{CodecError, Result, copy_match};

pub fn decompress(input: &[u8], out_len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(out_len.min(1 << 20));
    let mut i = 0usize;
    while i < input.len() && out.len() < out_len {
        let b = *input.get(i).ok_or(CodecError::Truncated)?;
        if b & 0x80 != 0 {
            let n = usize::from(b & 0x7f) + 1;
            let lit = input.get(i + 1..i + 1 + n).ok_or(CodecError::Truncated)?;
            let take = n.min(out_len - out.len());
            out.extend_from_slice(lit.get(..take).unwrap_or_default());
            i += 1 + n;
        } else if b & 0x40 != 0 {
            let hi = usize::from(*input.get(i + 1).ok_or(CodecError::Truncated)?);
            let lo = usize::from(*input.get(i + 2).ok_or(CodecError::Truncated)?);
            copy_match(&mut out, ((hi << 8) | lo) + 1, usize::from(b & 0x3f) + 4, out_len)?;
            i += 3;
        } else {
            let lo = usize::from(*input.get(i + 1).ok_or(CodecError::Truncated)?);
            let dist = ((usize::from(b & 3) << 8) | lo) + 1;
            copy_match(&mut out, dist, usize::from((b >> 2) & 0xf) + 3, out_len)?;
            i += 2;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_and_both_match_forms() {
        // Literal "abc", a three-byte match (length 4 + 4, distance 3), then a two-byte match
        // (length 1 + 3, distance 3): "abc" + "abcabcab" + "cabc".
        let c = [0x82, b'a', b'b', b'c', 0x40 | 4, 0x00, 0x02, 1 << 2, 0x02];
        let d = decompress(&c, 64).unwrap();
        assert_eq!(&d, b"abcabcabcabcabc");
        assert!(decompress(&[0x40, 0xFF, 0xFF], 10).is_err());
    }
}
