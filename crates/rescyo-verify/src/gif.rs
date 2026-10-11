//! GIF: header, logical screen, then blocks up to the trailer.

use std::io;

use crate::{Input, Verdict, read_at};

const MAX_BLOCKS: u32 = 10_000_000;

pub fn check(r: &mut dyn Input, len: u64) -> io::Result<Verdict> {
    let mut lsd = [0u8; 7];
    if len < 14 {
        return Ok(Verdict::damaged("truncated"));
    }
    read_at(r, 6, &mut lsd)?;
    let mut off = 13u64;
    if lsd[4] & 0x80 != 0 {
        off += 3 * (1u64 << ((lsd[4] & 7) + 1));
    }
    let byte = |r: &mut dyn Input, at: u64| -> io::Result<Option<u8>> {
        if at >= len {
            return Ok(None);
        }
        let mut b = [0u8; 1];
        read_at(r, at, &mut b)?;
        Ok(Some(b[0]))
    };
    // Skips a chain of data sub-blocks starting at `at`; returns the offset after the terminator.
    let sub_blocks = |r: &mut dyn Input, mut at: u64| -> io::Result<Option<u64>> {
        for _ in 0..MAX_BLOCKS {
            match byte(r, at)? {
                None => return Ok(None),
                Some(0) => return Ok(Some(at + 1)),
                Some(n) => at += 1 + u64::from(n),
            }
        }
        Ok(None)
    };
    let mut images = 0u32;
    for _ in 0..MAX_BLOCKS {
        match byte(r, off)? {
            None => return Ok(Verdict::damaged("truncated: no trailer")),
            Some(0x3B) => {
                return Ok(if images > 0 {
                    Verdict::Intact
                } else {
                    Verdict::damaged("no image")
                });
            }
            Some(0x21) => match sub_blocks(r, off + 2)? {
                Some(next) => off = next,
                None => return Ok(Verdict::damaged("truncated extension")),
            },
            Some(0x2C) => {
                let mut d = [0u8; 9];
                if off + 10 > len {
                    return Ok(Verdict::damaged("truncated image descriptor"));
                }
                read_at(r, off + 1, &mut d)?;
                let mut at = off + 10;
                if d[8] & 0x80 != 0 {
                    at += 3 * (1u64 << ((d[8] & 7) + 1));
                }
                match byte(r, at)? {
                    Some(m) if (2..=12).contains(&m) => {}
                    _ => return Ok(Verdict::damaged("bad LZW code size")),
                }
                match sub_blocks(r, at + 1)? {
                    Some(next) => off = next,
                    None => return Ok(Verdict::damaged("truncated image data")),
                }
                images += 1;
            }
            Some(b) => return Ok(Verdict::damaged(format!("unexpected block 0x{b:02x} at {off}"))),
        }
    }
    Ok(Verdict::damaged("too many blocks"))
}
