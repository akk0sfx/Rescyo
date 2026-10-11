//! A file's bytes from its [`DataLayout`]: disk runs read through the guarded reader, zeros,
//! inline data, and encoded extents decoded on the way.
//!
//! Damage never stops the stream: unreadable sectors are zeros, and every byte that may be
//! wrong because of them (a whole compression unit or chunk when its stored data is affected,
//! or when it fails to decode) is reported as an unreadable range of the file.

#![allow(clippy::single_range_in_vec_init, reason = "these are lists of byte ranges")]

use std::ops::Range;

use rescyo_codec::{lznt1, wof};
use rescyo_io::{GuardError, GuardedReader};

use crate::{Codec, DataLayout, Extent};

const CHUNK: usize = 1 << 20;
/// Largest LZNT1 unit decoded in memory (NTFS uses 64 KiB; anything far larger is hostile).
const MAX_UNIT: u64 = 16 << 20;

#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    /// The reader stopped (degradation alarm, source gone): recovery must stop too.
    #[error(transparent)]
    Guard(GuardError),
    /// The consumer failed (typically writing the target).
    #[error(transparent)]
    Sink(#[from] std::io::Error),
}

/// Receives the file's bytes in order; `unreadable` lists ranges (in file offsets) inside or
/// before this piece that are unreliable.
pub type Sink<'a> = dyn FnMut(&[u8], &[Range<u64>]) -> std::io::Result<()> + 'a;

struct Out<'a, 'b> {
    sink: &'a mut Sink<'b>,
    pos: u64,
    size: u64,
}

impl Out<'_, '_> {
    fn left(&self) -> u64 {
        self.size - self.pos.min(self.size)
    }

    /// Emits `data` (clipped to the size) with `bad` given relative to `data`.
    fn put(&mut self, data: &[u8], bad: &[Range<u64>]) -> Result<(), ContentError> {
        let n = usize::try_from((data.len() as u64).min(self.left())).unwrap_or(0);
        if n == 0 {
            return Ok(());
        }
        let end = n as u64;
        let abs: Vec<Range<u64>> = bad
            .iter()
            .filter(|r| r.start < end)
            .map(|r| self.pos + r.start..self.pos + r.end.min(end))
            .collect();
        (self.sink)(data.get(..n).unwrap_or_default(), &abs)?;
        self.pos += end;
        Ok(())
    }

    fn zeros(&mut self, len: u64, bad: bool) -> Result<(), ContentError> {
        let buf = vec![0u8; CHUNK.min(usize::try_from(len).unwrap_or(CHUNK)).max(1)];
        let mut left = len.min(self.left());
        while left > 0 {
            let n = left.min(buf.len() as u64);
            let b = if bad { vec![0..n] } else { Vec::new() };
            self.put(buf.get(..usize::try_from(n).unwrap_or(0)).unwrap_or_default(), &b)?;
            left -= n;
        }
        Ok(())
    }
}

/// Streams the first `size` bytes described by `layout`. A layout shorter than `size` is padded
/// with zeros reported as unreadable.
pub fn stream(
    r: &GuardedReader,
    layout: &DataLayout,
    size: u64,
    sink: &mut Sink<'_>,
) -> Result<(), ContentError> {
    let mut out = Out { sink, pos: 0, size };
    for x in &layout.extents {
        if out.left() == 0 {
            break;
        }
        match x {
            Extent::Disk { offset, len } => {
                let mut done = 0u64;
                let len = (*len).min(out.left());
                let mut buf = vec![0u8; CHUNK.min(usize::try_from(len).unwrap_or(CHUNK)).max(1)];
                while done < len {
                    let n = (len - done).min(buf.len() as u64);
                    let chunk = buf.get_mut(..usize::try_from(n).unwrap_or(0)).unwrap_or_default();
                    let bad = read(r, offset + done, chunk)?;
                    out.put(chunk, &bad)?;
                    done += n;
                }
            }
            Extent::Zero { len } => out.zeros(*len, false)?,
            Extent::Inline { data } => out.put(data, &[])?,
            Extent::Encoded {
                codec: Codec::Lznt1,
                stored,
                len,
            } => {
                let len = (*len).min(out.left());
                if len > MAX_UNIT || total(stored) > MAX_UNIT {
                    out.zeros(len, true)?;
                    continue;
                }
                let (raw, damaged) = gather(r, stored)?;
                let want = usize::try_from(len).unwrap_or(0);
                match lznt1::decompress(&raw, want) {
                    Ok(mut d) if !damaged => {
                        let short = d.len() < want;
                        d.resize(want, 0);
                        let bad = if short { vec![0..len] } else { Vec::new() };
                        out.put(&d, &bad)?;
                    }
                    Ok(mut d) => {
                        d.resize(want, 0);
                        out.put(&d, &[0..len])?;
                    }
                    Err(_) => out.zeros(len, true)?,
                }
            }
            Extent::Encoded { codec, stored, len } => {
                let algorithm = match codec {
                    Codec::WofXpress4k => wof::Algorithm::Xpress4k,
                    Codec::WofXpress8k => wof::Algorithm::Xpress8k,
                    Codec::WofXpress16k => wof::Algorithm::Xpress16k,
                    _ => wof::Algorithm::Lzx,
                };
                wof_stream(r, algorithm, stored, *len, &mut out)?;
            }
        }
    }
    let missing = out.left();
    out.zeros(missing, true)
}

fn total(stored: &[Extent]) -> u64 {
    stored.iter().map(Extent::len).sum()
}

/// Reads `buf` at `at`; returns the unreadable ranges relative to `buf`.
fn read(r: &GuardedReader, at: u64, buf: &mut [u8]) -> Result<Vec<Range<u64>>, ContentError> {
    match r.read(at, buf) {
        Ok(rep) => Ok(rep.unreadable.iter().map(|u| u.start - at..u.end - at).collect()),
        Err(GuardError::OutOfBounds) => {
            buf.fill(0);
            Ok(vec![0..buf.len() as u64])
        }
        Err(g) => Err(ContentError::Guard(g)),
    }
}

/// All bytes of `stored`, and whether any of them were unreadable.
fn gather(r: &GuardedReader, stored: &[Extent]) -> Result<(Vec<u8>, bool), ContentError> {
    let mut raw = Vec::new();
    let mut damaged = false;
    for s in stored {
        match s {
            Extent::Disk { offset, len } => {
                let at = raw.len();
                raw.resize(at + usize::try_from(*len).unwrap_or(0), 0);
                damaged |= !read(r, *offset, raw.get_mut(at..).unwrap_or_default())?.is_empty();
            }
            Extent::Zero { len } => raw.resize(raw.len() + usize::try_from(*len).unwrap_or(0), 0),
            Extent::Inline { data } => raw.extend_from_slice(data),
            Extent::Encoded { .. } => damaged = true,
        }
    }
    Ok((raw, damaged))
}

/// Random access into a stored stream described by plain extents.
struct Stored<'a> {
    r: &'a GuardedReader,
    extents: &'a [Extent],
    len: u64,
}

impl Stored<'_> {
    /// Reads `buf` at stream offset `at`; returns whether every byte was read cleanly.
    fn read_at(&self, mut at: u64, buf: &mut [u8]) -> Result<bool, ContentError> {
        let mut clean = true;
        let mut filled = 0usize;
        let mut base = 0u64;
        for x in self.extents {
            if filled == buf.len() {
                break;
            }
            let xlen = x.len();
            if at >= base + xlen {
                base += xlen;
                continue;
            }
            let within = at - base;
            let n = usize::try_from((xlen - within).min((buf.len() - filled) as u64)).unwrap_or(0);
            let dst = buf.get_mut(filled..filled + n).unwrap_or_default();
            match x {
                Extent::Disk { offset, .. } => clean &= read(self.r, offset + within, dst)?.is_empty(),
                Extent::Zero { .. } => dst.fill(0),
                Extent::Inline { data } => {
                    let w = usize::try_from(within).unwrap_or(0);
                    dst.copy_from_slice(data.get(w..w + n).unwrap_or_default());
                }
                Extent::Encoded { .. } => {
                    dst.fill(0);
                    clean = false;
                }
            }
            filled += n;
            at += n as u64;
            base += xlen;
        }
        if filled < buf.len() {
            buf.get_mut(filled..).unwrap_or_default().fill(0);
            clean = false;
        }
        Ok(clean)
    }
}

fn wof_stream(
    r: &GuardedReader,
    algorithm: wof::Algorithm,
    stored: &[Extent],
    len: u64,
    out: &mut Out<'_, '_>,
) -> Result<(), ContentError> {
    let layout = wof::Layout::new(algorithm, len);
    let s = Stored {
        r,
        extents: stored,
        len: total(stored),
    };
    for i in 0..layout.chunks {
        if out.left() == 0 {
            break;
        }
        let want = layout.chunk_len(i);
        let er = layout.entries_for(i);
        let mut entries = vec![0u8; usize::try_from(er.end - er.start).unwrap_or(0)];
        let mut clean = s.read_at(er.start, &mut entries)?;
        let decoded = layout.chunk_range(i, &entries, s.len).ok().and_then(|cr| {
            let n = usize::try_from(cr.end - cr.start).ok()?;
            if n as u64 > want {
                return None;
            }
            let mut data = vec![0u8; n];
            match s.read_at(cr.start, &mut data) {
                Ok(c) => clean &= c,
                Err(e) => return Some(Err(e)),
            }
            layout.decode(i, &data).ok().map(Ok)
        });
        match decoded {
            Some(Ok(d)) if d.len() as u64 == want => {
                let bad = if clean { Vec::new() } else { vec![0..want] };
                out.put(&d, &bad)?;
            }
            Some(Err(e)) => return Err(e),
            _ => out.zeros(want, true)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rescyo_io::ReadPolicy;
    use rescyo_io::testing::{FaultySource, MemSource};

    use super::*;

    fn collect(r: &GuardedReader, l: &DataLayout, size: u64) -> (Vec<u8>, Vec<Range<u64>>) {
        let (mut data, mut bad) = (Vec::new(), Vec::new());
        stream(r, l, size, &mut |d, b| {
            data.extend_from_slice(d);
            bad.extend_from_slice(b);
            Ok(())
        })
        .unwrap();
        (data, bad)
    }

    #[test]
    fn plain_extents_zeros_inline_and_padding() {
        let mut disk = vec![0u8; 8192];
        disk[1024..1028].copy_from_slice(b"ABCD");
        let r = GuardedReader::new(Arc::new(MemSource::new(disk, 512)), ReadPolicy::default());
        let l = DataLayout {
            extents: vec![
                Extent::Disk { offset: 1024, len: 4 },
                Extent::Zero { len: 2 },
                Extent::Inline { data: b"xy".to_vec() },
            ],
        };
        let (d, bad) = collect(&r, &l, 10);
        assert_eq!(d, b"ABCD\0\0xy\0\0");
        assert_eq!(bad, vec![8..10]);
    }

    #[test]
    fn lznt1_units_decode_and_damage_marks_the_whole_unit() {
        // "abc" compressed as one chunk, stored in sector 2; the unit is 4096 bytes.
        let mut disk = vec![0u8; 8192];
        disk[1024..1030].copy_from_slice(&[0x03, 0xB0, 0x00, b'a', b'b', b'c']);
        let unit = DataLayout {
            extents: vec![Extent::Encoded {
                codec: Codec::Lznt1,
                stored: vec![Extent::Disk {
                    offset: 1024,
                    len: 512,
                }],
                len: 4096,
            }],
        };
        let good = GuardedReader::new(Arc::new(MemSource::new(disk.clone(), 512)), ReadPolicy::default());
        let (d, bad) = collect(&good, &unit, 4000);
        assert_eq!(&d[..3], b"abc");
        assert_eq!(d.len(), 4000);
        assert!(bad.is_empty());
        let src = FaultySource::new(MemSource::new(disk, 512)).bad(2..3);
        let broken = GuardedReader::new(Arc::new(src), ReadPolicy::default());
        let (d, bad) = collect(&broken, &unit, 4000);
        assert_eq!(d.len(), 4000);
        assert_eq!(bad, vec![0..4000]);
    }
}
