//! Any bytes as LZNT1, XPRESS Huffman, LZX, a WOF chunk table, ADC, bzip2, LZFSE and xz: decoding must never panic,
//! never produce more than asked, and stay within its time bound.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rescyo_codec::wof::{Algorithm, Layout};
use rescyo_codec::{adc, bzip2, lznt1, lzfse, lzx, xpress, xz};

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    let want = 1 + usize::from(sel >> 3) * 2048;
    let out = match sel & 7 {
        0 => lznt1::decompress(rest, want.min(65536)),
        1 => xpress::decompress(rest, want.min(65536)),
        2 => lzx::decompress(rest, want.min(32768)),
        4 => adc::decompress(rest, want),
        5 => bzip2::decompress(rest, want),
        6 => lzfse::decompress(rest, want),
        7 => xz::decompress(rest, want),
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
        assert!(o.len() <= want);
    }
});
