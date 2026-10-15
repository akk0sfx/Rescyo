//! bzip2 streams written by the reference library (libbzip2 through Python's bz2), decoded
//! byte for byte; damage is reported by the block CRC.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "integration test")]

use rescyo_codec::bzip2::decompress;
use sha2::{Digest, Sha256};

fn sha(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn reference_streams_decode_exactly() {
    for (data, len, want) in [
        (
            &include_bytes!("vectors/text.bz2")[..],
            231_274,
            "3ab8080f9711a038c455009cf9303b2cc56d82669cd69a01b8cb4b8db4d5910e",
        ),
        (
            &include_bytes!("vectors/runs.bz2")[..],
            170_228,
            "befc4c67b9f5312f535120b3226fab847f6a6c7221238f470c2a1cca83f85b1e",
        ),
    ] {
        let out = decompress(data, len).unwrap();
        assert_eq!(out.len(), len);
        assert_eq!(sha(&out), want);
    }
    let m = include_bytes!("vectors/multiblock.bz2");
    let out = decompress(m, 10 << 20).unwrap();
    assert_eq!(out.len(), 405_502, "several level-1 blocks");
    assert_eq!(
        sha(&out),
        "3e3bbad25ab1d497d253ab196c1341334a4d978f4bfe700e1b6252dce0f1645a"
    );
}

#[test]
fn damage_is_detected_not_returned() {
    let mut d = include_bytes!("vectors/text.bz2").to_vec();
    let mid = d.len() / 2;
    d[mid] ^= 0x10;
    assert!(decompress(&d, 231_274).is_err());
    assert!(decompress(&d[..d.len() / 3], 231_274).is_err());
    assert!(decompress(b"BZh9", 10).is_err());
}
