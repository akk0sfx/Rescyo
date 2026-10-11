//! PNG: every chunk carries a CRC-32; the file must end with IEND.

use std::io;

use crate::{Input, Verdict, read_at};

const MAX_CHUNKS: u32 = 1_000_000;

pub fn check(r: &mut dyn Input, len: u64) -> io::Result<Verdict> {
    let mut off = 8u64;
    let mut first = true;
    let mut buf = Vec::new();
    for _ in 0..MAX_CHUNKS {
        if off + 12 > len {
            return Ok(Verdict::damaged("truncated: no IEND"));
        }
        let mut h = [0u8; 8];
        read_at(r, off, &mut h)?;
        let clen = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
        let kind = [h[4], h[5], h[6], h[7]];
        if !kind.iter().all(u8::is_ascii_alphabetic) {
            return Ok(Verdict::damaged(format!("bad chunk type at {off}")));
        }
        if first && &kind != b"IHDR" {
            return Ok(Verdict::damaged("first chunk is not IHDR"));
        }
        first = false;
        if off + 12 + clen > len {
            return Ok(Verdict::damaged(format!(
                "chunk {} runs past the end",
                String::from_utf8_lossy(&kind)
            )));
        }
        // CRC over type and data, streamed in bounded pieces.
        let mut crc = crc32fast::Hasher::new();
        crc.update(&kind);
        let mut left = clen;
        let mut pos = off + 8;
        while left > 0 {
            let n = usize::try_from(left.min(1 << 20)).unwrap_or(1 << 20);
            buf.resize(n, 0);
            read_at(r, pos, &mut buf)?;
            crc.update(&buf);
            pos += n as u64;
            left -= n as u64;
        }
        let mut stored = [0u8; 4];
        read_at(r, pos, &mut stored)?;
        if crc.finalize() != u32::from_be_bytes(stored) {
            return Ok(Verdict::damaged(format!(
                "CRC mismatch in {} chunk at {off}",
                String::from_utf8_lossy(&kind)
            )));
        }
        off = pos + 4;
        if &kind == b"IEND" {
            return Ok(Verdict::Intact);
        }
    }
    Ok(Verdict::damaged("too many chunks"))
}
