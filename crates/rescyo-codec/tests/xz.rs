//! xz streams written by liblzma (through Python's lzma module), decoded byte for byte: CRC64,
//! CRC32, no check and SHA-256; several LZMA2 chunks, stored chunks and unusual lc/lp/pb.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "integration test")]

use rescyo_codec::xz::decompress;
use sha2::{Digest, Sha256};

fn sha(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

const TEXT_LEN: usize = 2_250_217;
const TEXT: &str = "88ee8cf16fa9fe7bf000914e7567772b0b56c9236d061feebff3703e6d06b600";
const MIXED: &str = "0b997c2defc7f1865c7f152731f9e6b917262986923b193e818bbe3246cb2d67";

const VECTORS: [(&[u8], usize, &str); 4] = [
    (include_bytes!("vectors/text-crc64.xz"), TEXT_LEN, TEXT),
    (include_bytes!("vectors/mixed-crc32.xz"), 270_000, MIXED),
    (
        include_bytes!("vectors/noise-none.xz"),
        150_000,
        "7b5d40853fada148e930c72389369bac67c77727f90dc23bd3a0dede8b2d312b",
    ),
    (include_bytes!("vectors/lp2-sha256.xz"), 270_000, MIXED),
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
fn damage_is_detected() {
    for (data, len, _) in VECTORS {
        assert!(decompress(data, len - 1).is_err(), "bounded by the caller");
        assert!(decompress(&data[..data.len() - 13], len).is_err());
    }
    // CRC-protected streams: any flipped bit is an error, never wrong output.
    for (data, len, _) in &VECTORS[..2] {
        for at in (12..data.len() - 12).step_by(data.len() / 48) {
            let mut d = data.to_vec();
            d[at] ^= 0x21;
            assert!(decompress(&d, *len).is_err(), "flip at {at}");
        }
    }
}
