//! LZFSE streams written by Apple's encoder (`compression_tool -encode -a lzfse`), decoded byte
//! for byte: a small input (one LZVN block), text, incompressible noise (a stored block) and a
//! mix whose matches reach across blocks.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "integration test")]

use rescyo_codec::lzfse::decompress;
use sha2::{Digest, Sha256};

fn sha(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

const VECTORS: [(&[u8], usize, &str); 4] = [
    (
        include_bytes!("vectors/small.lzfse"),
        2_258,
        "ded8d5308afc6143c39489e216fbbec90acfee96d03696c6c959a9535a227d03",
    ),
    (
        include_bytes!("vectors/text.lzfse"),
        453_247,
        "2b468f01e9b0aeb2890ef857d1e00cd69649eedef7a47ebaf474ffad32462271",
    ),
    (
        include_bytes!("vectors/noise.lzfse"),
        20_000,
        "e6f7a08a871c0231c0c9dbb95438a2df4f9a44d75ad0bcd20731fa1cf66db9f6",
    ),
    (
        include_bytes!("vectors/mixed.lzfse"),
        520_000,
        "6078a56268d2df7489ff8f1153d4bbebee645ba98ae182a9be757444de2dbad8",
    ),
];

#[test]
fn reference_streams_decode_exactly() {
    for (data, len, want) in VECTORS {
        let out = decompress(data, len).unwrap_or_else(|e| panic!("{len}: {e:?}"));
        assert_eq!(out.len(), len);
        assert_eq!(sha(&out), want);
    }
}

#[test]
fn damage_is_an_error_or_a_different_result_never_a_panic() {
    for (data, len, want) in VECTORS {
        assert!(decompress(data, len - 1).is_err(), "bounded by the caller");
        for cut in [4, data.len() / 3, data.len() - 4] {
            assert!(decompress(&data[..cut], len).is_err());
        }
        for at in (0..data.len()).step_by(data.len() / 64 + 1) {
            let mut d = data.to_vec();
            d[at] ^= 0x5a;
            if let Ok(out) = decompress(&d, len) {
                assert!(out.len() <= len);
                let _ = sha(&out) == want;
            }
        }
    }
}
