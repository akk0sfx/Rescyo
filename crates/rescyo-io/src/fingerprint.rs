//! Source fingerprints: telling whether a Session belongs to the Source in front of us.

use sha2::{Digest, Sha256};

use crate::{GuardError, GuardedReader};

const HEAD: u64 = 1 << 20;
const MIDDLE: u64 = 64 << 10;
const TAIL: u64 = 1 << 20;

/// A hashed sample region. `sha256` is `None` when part of the region was unreadable, which
/// makes the sample neutral in comparisons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub offset: u64,
    pub len: u64,
    pub sha256: Option<[u8; 32]>,
}

/// The identity of a Source: geometry, hardware identity when known, and sample hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFingerprint {
    pub size: u64,
    pub logical_sector: u32,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub samples: Vec<Sample>,
}

/// Outcome of comparing two fingerprints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintMatch {
    /// Same geometry and every comparable sample agrees (at least one was comparable).
    Match,
    /// Definitely a different Source (or changed content).
    Mismatch(&'static str),
    /// Geometry agrees but no sample could be compared: needs a forced rebind.
    Unverifiable,
}

impl SourceFingerprint {
    /// Reads the sample regions through `reader` (honouring its Bad-sector map).
    pub fn compute(reader: &GuardedReader) -> Result<Self, GuardError> {
        let g = reader.geometry();
        let id = reader.identity();
        let mut samples = Vec::new();
        for (offset, len) in sample_regions(g.size, u64::from(g.logical_sector)) {
            let mut buf = vec![0u8; usize::try_from(len).map_err(|_| GuardError::OutOfBounds)?];
            let report = reader.read(offset, &mut buf)?;
            let sha256 = report.is_clean().then(|| Sha256::digest(&buf).into());
            samples.push(Sample { offset, len, sha256 });
        }
        Ok(Self {
            size: g.size,
            logical_sector: g.logical_sector,
            model: id.model.clone(),
            serial: id.serial.clone(),
            samples,
        })
    }

    /// Compares a stored fingerprint (`self`) with a freshly computed one.
    pub fn compare(&self, other: &Self) -> FingerprintMatch {
        if self.size != other.size {
            return FingerprintMatch::Mismatch("size differs");
        }
        if self.logical_sector != other.logical_sector {
            return FingerprintMatch::Mismatch("sector size differs");
        }
        if let (Some(a), Some(b)) = (&self.serial, &other.serial)
            && a != b
        {
            return FingerprintMatch::Mismatch("hardware serial differs");
        }
        if self.samples.len() != other.samples.len() {
            return FingerprintMatch::Mismatch("sample layout differs");
        }
        let mut compared = 0;
        for (a, b) in self.samples.iter().zip(&other.samples) {
            if a.offset != b.offset || a.len != b.len {
                return FingerprintMatch::Mismatch("sample layout differs");
            }
            if let (Some(x), Some(y)) = (a.sha256, b.sha256) {
                if x != y {
                    return FingerprintMatch::Mismatch("content differs");
                }
                compared += 1;
            }
        }
        if compared == 0 {
            FingerprintMatch::Unverifiable
        } else {
            FingerprintMatch::Match
        }
    }
}

/// Head (partition tables, boot sectors), middle and tail (backup GPT, NTFS backup boot sector).
fn sample_regions(size: u64, ss: u64) -> Vec<(u64, u64)> {
    let align = |v: u64| v - v % ss;
    if size <= HEAD + MIDDLE + TAIL {
        return if size == 0 { Vec::new() } else { vec![(0, size)] };
    }
    let mid = align(size / 2);
    let tail = align(size - TAIL);
    vec![(0, HEAD), (mid, MIDDLE), (tail, size - tail)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FaultySource, MemSource, pattern};
    use crate::{ReadPolicy, SourceIdentity, SourceKind};
    use std::sync::Arc;

    const LEN: usize = 4 << 20;

    fn fp_of(data: Vec<u8>) -> SourceFingerprint {
        let r = GuardedReader::new(Arc::new(MemSource::new(data, 512)), ReadPolicy::default());
        SourceFingerprint::compute(&r).unwrap()
    }

    #[test]
    fn identical_content_matches() {
        assert_eq!(
            fp_of(pattern(LEN)).compare(&fp_of(pattern(LEN))),
            FingerprintMatch::Match
        );
    }

    #[test]
    fn changed_head_or_tail_mismatches_but_untouched_bytes_do_not_matter() {
        let base = fp_of(pattern(LEN));
        let mut head = pattern(LEN);
        head[100] ^= 1;
        assert_eq!(
            base.compare(&fp_of(head)),
            FingerprintMatch::Mismatch("content differs")
        );
        let mut tail = pattern(LEN);
        tail[LEN - 1] ^= 1;
        assert!(matches!(
            base.compare(&fp_of(tail)),
            FingerprintMatch::Mismatch(_)
        ));
        let mut elsewhere = pattern(LEN);
        elsewhere[LEN / 4] ^= 1; // outside every sample region
        assert_eq!(base.compare(&fp_of(elsewhere)), FingerprintMatch::Match);
    }

    #[test]
    fn size_mismatch_is_detected() {
        let a = fp_of(pattern(LEN));
        let b = fp_of(pattern(LEN + 512));
        assert_eq!(a.compare(&b), FingerprintMatch::Mismatch("size differs"));
    }

    #[test]
    fn unreadable_sample_is_neutral() {
        let base = fp_of(pattern(LEN));
        let src = FaultySource::new(MemSource::new(pattern(LEN), 512)).bad(10..11);
        let r = GuardedReader::new(Arc::new(src), ReadPolicy::default());
        let damaged = SourceFingerprint::compute(&r).unwrap();
        assert!(damaged.samples[0].sha256.is_none());
        assert_eq!(base.compare(&damaged), FingerprintMatch::Match);
    }

    #[test]
    fn nothing_comparable_is_unverifiable() {
        let small = 64 * 1024;
        let base = fp_of(pattern(small));
        let src = FaultySource::new(MemSource::new(pattern(small), 512)).bad(0..1);
        let r = GuardedReader::new(Arc::new(src), ReadPolicy::default());
        assert_eq!(
            base.compare(&SourceFingerprint::compute(&r).unwrap()),
            FingerprintMatch::Unverifiable
        );
    }

    #[test]
    fn different_serials_mismatch() {
        let id = |s: &str| SourceIdentity {
            kind: SourceKind::PhysicalDisk,
            path: "d".into(),
            model: None,
            serial: Some(s.into()),
        };
        let mk = |s| {
            let src = MemSource::new(pattern(LEN), 512).with_identity(id(s));
            SourceFingerprint::compute(&GuardedReader::new(Arc::new(src), ReadPolicy::default())).unwrap()
        };
        assert_eq!(
            mk("A").compare(&mk("B")),
            FingerprintMatch::Mismatch("hardware serial differs")
        );
        // An image (no serial) of the same disk is still comparable by content.
        assert_eq!(mk("A").compare(&fp_of(pattern(LEN))), FingerprintMatch::Match);
    }
}
