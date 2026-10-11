//! Any bytes as LZNT1, XPRESS Huffman, LZX and a WOF chunk table: decoding must never panic,
//! never produce more than asked, and stay within its time bound.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rescyo_codec::wof::{Algorithm, Layout};
use rescyo_codec::{lznt1, lzx, xpress};

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    let want = 1 + usize::from(sel >> 2) * 1024;
    let out = match sel & 3 {
        0 => lznt1::decompress(rest, want.min(65536)),
        1 => xpress::decompress(rest, want.min(65536)),
        2 => lzx::decompress(rest, want.min(32768)),
        _ => {
            let l = Layout::new(Algorithm::Xpress4k, rest.len() as u64 * 3);
            for i in 0..l.chunks.min(64) {
                let e = l.entries_for(i);
                if let Some(t) = rest.get(e.start as usize..e.end as usize)
                    && let Ok(r) = l.chunk_range(i, t, rest.len() as u64)
                    && let Some(c) = rest.get(r.start as usize..r.end as usize)
                {
                    let _ = l.decode(i, c);
                }
            }
            return;
        }
    };
    if let Ok(o) = out {
        assert!(o.len() <= want.min(65536));
    }
});
