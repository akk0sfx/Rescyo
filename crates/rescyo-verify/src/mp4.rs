//! MP4 / QuickTime / HEIF: top-level boxes must tile the file exactly and include the movie
//! (or meta) box; nested container boxes must tile their parent.

use std::io;

use crate::{Input, Verdict, read_at};

const MAX_BOXES: u32 = 1_000_000;
const MAX_DEPTH: u32 = 8;
const CONTAINERS: [&[u8; 4]; 8] = [
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"dinf", b"udta",
];

pub fn check(r: &mut dyn Input, len: u64) -> io::Result<Verdict> {
    let mut count = 0u32;
    let mut top = Vec::new();
    if let Err(why) = walk(r, 0, len, 0, &mut count, &mut top)? {
        return Ok(Verdict::damaged(why));
    }
    let has = |t: &[u8; 4]| top.iter().any(|x| x == t);
    if has(b"moov") || has(b"meta") || has(b"moof") {
        Ok(Verdict::Intact)
    } else {
        Ok(Verdict::damaged("no movie or meta box"))
    }
}

type Walk = Result<(), String>;

fn walk(
    r: &mut dyn Input,
    start: u64,
    end: u64,
    depth: u32,
    count: &mut u32,
    top: &mut Vec<[u8; 4]>,
) -> io::Result<Walk> {
    let mut off = start;
    while off < end {
        *count += 1;
        if *count > MAX_BOXES {
            return Ok(Err("too many boxes".into()));
        }
        if off + 8 > end {
            return Ok(Err(format!("box header past the end at {off}")));
        }
        let mut h = [0u8; 8];
        read_at(r, off, &mut h)?;
        let mut size = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
        let kind = [h[4], h[5], h[6], h[7]];
        if !kind
            .iter()
            .all(|b| b.is_ascii_graphic() || *b == b' ' || *b == 0xA9)
        {
            return Ok(Err(format!("bad box type at {off}")));
        }
        let mut header = 8;
        if size == 1 {
            let mut big = [0u8; 8];
            if off + 16 > end {
                return Ok(Err("truncated large box".into()));
            }
            read_at(r, off + 8, &mut big)?;
            size = u64::from_be_bytes(big);
            header = 16;
        } else if size == 0 {
            size = end - off;
        }
        if size < header || off.checked_add(size).is_none_or(|e| e > end) {
            return Ok(Err(format!(
                "box {} at {off} runs past its parent",
                String::from_utf8_lossy(&kind)
            )));
        }
        if depth == 0 {
            top.push(kind);
        }
        if depth < MAX_DEPTH
            && CONTAINERS.contains(&&kind)
            && let Err(e) = walk(r, off + header, off + size, depth + 1, count, top)?
        {
            return Ok(Err(e));
        }
        off += size;
    }
    Ok(Ok(()))
}
