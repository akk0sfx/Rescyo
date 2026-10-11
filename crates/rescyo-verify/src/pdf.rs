//! PDF: header, an end-of-file marker near the end, and a cross-reference that points at real
//! objects (classic tables are sampled; cross-reference streams must be an object).

use std::io;

use crate::{Input, Verdict, read_at};

const TAIL: u64 = 4096;
const SAMPLE: usize = 64;

pub fn check(r: &mut dyn Input, len: u64) -> io::Result<Verdict> {
    let n = len.min(TAIL);
    let mut tail = vec![0u8; usize::try_from(n).unwrap_or(0)];
    read_at(r, len - n, &mut tail)?;
    let t = String::from_utf8_lossy(&tail);
    let Some(eof) = t.rfind("%%EOF") else {
        return Ok(Verdict::damaged("no %%EOF marker"));
    };
    let Some(sx) = t.get(..eof).and_then(|s| s.rfind("startxref")) else {
        return Ok(Verdict::damaged("no startxref"));
    };
    let Some(xref) = t
        .get(sx + 9..eof)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<u64>().ok())
    else {
        return Ok(Verdict::damaged("bad startxref offset"));
    };
    if xref >= len {
        return Ok(Verdict::damaged("startxref points past the end"));
    }
    let mut at = vec![0u8; usize::try_from((len - xref).min(1 << 16)).unwrap_or(0)];
    read_at(r, xref, &mut at)?;
    let s = String::from_utf8_lossy(&at);
    if s.starts_with("xref") {
        // Sample entries of the first subsection: each in-use offset must start "N G obj".
        let mut lines = s.lines().skip(1);
        let Some(head) = lines.next() else {
            return Ok(Verdict::damaged("empty xref"));
        };
        let mut parts = head.split_whitespace();
        let first: u64 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let count: usize = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        for (i, line) in lines.take(count.min(SAMPLE)).enumerate() {
            let f: Vec<&str> = line.split_whitespace().collect();
            let (Some(&offset), Some(&kind)) = (f.first(), f.get(2)) else {
                return Ok(Verdict::damaged("malformed xref entry"));
            };
            if kind != "n" {
                continue;
            }
            let Ok(off) = offset.parse::<u64>() else {
                return Ok(Verdict::damaged("malformed xref offset"));
            };
            if !object_at(r, len, off, first + i as u64)? {
                return Ok(Verdict::damaged(format!(
                    "object {} is not where the xref says",
                    first + i as u64
                )));
            }
        }
        Ok(Verdict::Intact)
    } else if s.split_whitespace().nth(2) == Some("obj") || s.contains(" obj") {
        Ok(Verdict::Intact)
    } else {
        Ok(Verdict::damaged("startxref does not point at a cross-reference"))
    }
}

fn object_at(r: &mut dyn Input, len: u64, off: u64, num: u64) -> io::Result<bool> {
    if off >= len {
        return Ok(false);
    }
    let mut b = vec![0u8; usize::try_from((len - off).min(64)).unwrap_or(0)];
    read_at(r, off, &mut b)?;
    let s = String::from_utf8_lossy(&b);
    let mut w = s.split_whitespace();
    Ok(w.next() == Some(&num.to_string())
        && w.next().is_some()
        && w.next().is_some_and(|x| x.starts_with("obj")))
}
