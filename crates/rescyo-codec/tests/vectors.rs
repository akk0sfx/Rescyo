//! Streams produced by wimlib (the reference WIM/WOF implementation) in the corpus container,
//! decoded chunk by chunk and checked against the original's SHA-256.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "integration test"
)]

use rescyo_codec::wof::{Algorithm, Layout};
use sha2::{Digest, Sha256};

fn decode(stream: &[u8], algorithm: Algorithm, size: u64) -> String {
    let l = Layout::new(algorithm, size);
    let mut out = Vec::new();
    for i in 0..l.chunks {
        let e = l.entries_for(i);
        let r = l
            .chunk_range(i, &stream[e.start as usize..e.end as usize], stream.len() as u64)
            .unwrap();
        out.extend(l.decode(i, &stream[r.start as usize..r.end as usize]).unwrap());
    }
    assert_eq!(out.len() as u64, size);
    Sha256::digest(&out).iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn xpress_8k_from_wimlib() {
    let s = include_bytes!("vectors/xpress8k-50001.wof");
    assert_eq!(
        decode(s, Algorithm::Xpress8k, 50_001),
        "0249538ef45f32d83a0c4ee7ffa997958750dd98829db3419331c22b9f24d3fc"
    );
}

#[test]
fn lzx_from_wimlib() {
    let s = include_bytes!("vectors/lzx-150000.wof");
    assert_eq!(
        decode(s, Algorithm::Lzx, 150_000),
        "9e1b000906cd5c2991f19a4dc999efce153fa1defde2ff729bb12904bd649635"
    );
}

#[test]
fn truncated_or_flipped_streams_never_panic() {
    for (s, alg, size) in [
        (
            &include_bytes!("vectors/xpress8k-50001.wof")[..],
            Algorithm::Xpress8k,
            50_001u64,
        ),
        (
            &include_bytes!("vectors/lzx-150000.wof")[..],
            Algorithm::Lzx,
            150_000,
        ),
    ] {
        let l = Layout::new(alg, size);
        for cut in [1usize, 100, 1000, s.len() / 2] {
            let mut m = s.to_vec();
            m.truncate(s.len() - cut);
            let at = cut % m.len();
            m[at] ^= 0x5A;
            for i in 0..l.chunks {
                let e = l.entries_for(i);
                if let Some(t) = m.get(e.start as usize..e.end as usize)
                    && let Ok(r) = l.chunk_range(i, t, m.len() as u64)
                {
                    let _ = l.decode(i, &m[r.start as usize..r.end as usize]);
                }
            }
        }
    }
}
