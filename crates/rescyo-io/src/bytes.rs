//! Bounds-checked little-endian field access for parsers of on-disk structures.
//!
//! Every accessor returns `None` instead of panicking when the field does not fit, so parsers
//! can treat a short or truncated structure as invalid input.

#[inline]
fn array<const N: usize>(buf: &[u8], off: usize) -> Option<[u8; N]> {
    buf.get(off..off.checked_add(N)?)?.try_into().ok()
}

#[inline]
pub fn u8_at(buf: &[u8], off: usize) -> Option<u8> {
    buf.get(off).copied()
}

#[inline]
pub fn u16_at(buf: &[u8], off: usize) -> Option<u16> {
    array(buf, off).map(u16::from_le_bytes)
}

#[inline]
pub fn u32_at(buf: &[u8], off: usize) -> Option<u32> {
    array(buf, off).map(u32::from_le_bytes)
}

#[inline]
pub fn u64_at(buf: &[u8], off: usize) -> Option<u64> {
    array(buf, off).map(u64::from_le_bytes)
}

#[inline]
pub fn i8_at(buf: &[u8], off: usize) -> Option<i8> {
    buf.get(off).map(|&b| i8::from_le_bytes([b]))
}

#[inline]
pub fn bytes_at<const N: usize>(buf: &[u8], off: usize) -> Option<[u8; N]> {
    array(buf, off)
}

/// Big-endian `u16` (VHD, qcow2 and DMG store their fields big-endian).
#[inline]
pub fn be16_at(buf: &[u8], off: usize) -> Option<u16> {
    array(buf, off).map(u16::from_be_bytes)
}

/// Big-endian `u32`.
#[inline]
pub fn be32_at(buf: &[u8], off: usize) -> Option<u32> {
    array(buf, off).map(u32::from_be_bytes)
}

/// Big-endian `u64`.
#[inline]
pub fn be64_at(buf: &[u8], off: usize) -> Option<u64> {
    array(buf, off).map(u64::from_be_bytes)
}

/// A sub-slice that must lie entirely within `buf`.
#[inline]
pub fn slice_at(buf: &[u8], off: usize, len: usize) -> Option<&[u8]> {
    buf.get(off..off.checked_add(len)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_and_refuses_overruns() {
        let b = [1u8, 2, 3, 4, 5, 6, 7, 8, 0xFF];
        assert_eq!(u16_at(&b, 0), Some(0x0201));
        assert_eq!(u32_at(&b, 1), Some(0x0504_0302));
        assert_eq!(u64_at(&b, 0), Some(0x0807_0605_0403_0201));
        assert_eq!(i8_at(&b, 8), Some(-1));
        assert_eq!(u64_at(&b, 2), None);
        assert_eq!(u16_at(&b, usize::MAX), None);
        assert_eq!(slice_at(&b, 7, 2), Some(&b[7..9]));
        assert_eq!(slice_at(&b, 8, 2), None);
        assert_eq!(be16_at(&b, 0), Some(0x0102));
        assert_eq!(be32_at(&b, 0), Some(0x0102_0304));
        assert_eq!(be64_at(&b, 0), Some(0x0102_0304_0506_0708));
        assert_eq!(be64_at(&b, 2), None);
    }
}
