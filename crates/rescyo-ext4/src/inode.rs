//! Inodes and their data: extent trees, block maps and inline data.

use std::collections::HashSet;

use rescyo_fs::{DataLayout, Extent, Times, Timestamp};
use rescyo_io::bytes::{slice_at, u16_at, u32_at};
use rescyo_io::{GuardError, GuardedReader};

use crate::sb::Ext4Geometry;

const EXTENT_MAGIC: u16 = 0xF30A;
const FLAG_INDEX: u32 = 0x1000;
const FLAG_EXTENTS: u32 = 0x8_0000;
const FLAG_INLINE: u32 = 0x1000_0000;
const MAX_DEPTH: u16 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Other,
}

#[derive(Debug, Clone)]
pub struct Inode {
    pub kind: Kind,
    pub size: u64,
    pub links: u16,
    pub dtime: u32,
    pub flags: u32,
    pub times: Times,
    /// The 60 bytes of `i_block`: extent root, block pointers or inline data.
    pub block: [u8; 60],
}

impl Inode {
    pub fn parse(b: &[u8], inode_size: u32) -> Option<Self> {
        let mode = u16_at(b, 0)?;
        let kind = match mode & 0xF000 {
            0x8000 => Kind::File,
            0x4000 => Kind::Dir,
            _ => Kind::Other,
        };
        let size = u64::from(u32_at(b, 4)?) | (u64::from(u32_at(b, 108)?) << 32);
        let mut block = [0u8; 60];
        block.copy_from_slice(slice_at(b, 40, 60)?);
        // Extra fields (nanoseconds, creation time) when the inode is large enough.
        let extra = if inode_size > 128 {
            u32::from(u16_at(b, 0x80).unwrap_or(0))
        } else {
            0
        };
        let time = |secs_at: usize, extra_at: Option<usize>| {
            let secs = i64::from(u32_at(b, secs_at)?.cast_signed());
            let (epoch, nanos) = match extra_at.filter(|&o| o + 4 <= 0x80 + extra as usize) {
                Some(o) => {
                    let x = u32_at(b, o)?;
                    (i64::from(x & 3), x >> 2)
                }
                None => (0, 0),
            };
            Timestamp::from_unix(secs + (epoch << 32), nanos)
        };
        Some(Self {
            kind,
            size,
            links: u16_at(b, 26)?,
            dtime: u32_at(b, 20)?,
            flags: u32_at(b, 32)?,
            times: Times {
                created: time(0x90, Some(0x94)),
                modified: time(16, Some(0x88)),
                accessed: time(8, Some(0x8C)),
            },
            block,
        })
    }

    pub fn deleted(&self) -> bool {
        self.dtime != 0 || self.links == 0
    }

    pub fn hashed_dir(&self) -> bool {
        self.flags & FLAG_INDEX != 0
    }
}

/// Reads inode `n` (1-based).
pub fn read(
    r: &GuardedReader,
    g: &Ext4Geometry,
    tables: &[Option<u64>],
    n: u64,
) -> Result<Option<Inode>, GuardError> {
    if n == 0 || n > u64::from(g.inodes) {
        return Ok(None);
    }
    let ipg = u64::from(g.inodes_per_group);
    let group = usize::try_from((n - 1) / ipg).unwrap_or(usize::MAX);
    let Some(Some(table)) = tables.get(group) else {
        return Ok(None);
    };
    let isz = u64::from(g.inode_size);
    let Some(base) = g.block_offset(*table) else {
        return Ok(None);
    };
    let at = base + ((n - 1) % ipg) * isz;
    let mut b = vec![0u8; usize::try_from(isz).unwrap_or(256)];
    // Inodes are not sector-sized: read the enclosing sectors.
    let ss = u64::from(r.geometry().logical_sector);
    let start = at - at % ss;
    let end = (at + isz).div_ceil(ss) * ss;
    let mut w = vec![0u8; usize::try_from(end - start).unwrap_or(0)];
    match r.read(start, &mut w) {
        Ok(rep) if rep.is_clean() => {}
        Ok(_) | Err(GuardError::OutOfBounds) => return Ok(None),
        Err(e) => return Err(e),
    }
    let skip = usize::try_from(at - start).unwrap_or(0);
    let n = b.len();
    let Some(src) = w.get(skip..skip + n) else {
        return Ok(None);
    };
    b.copy_from_slice(src);
    Ok(Inode::parse(&b, g.inode_size))
}

/// How the data was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Extents,
    BlockMap,
    Inline,
}

/// The layout of an inode's data, trimmed to its size. `stale` allows a deleted inode's
/// emptied extent root to be read from the entries it still holds.
pub fn layout(
    r: &GuardedReader,
    g: &Ext4Geometry,
    ino: &Inode,
    max_extents: u32,
    stale: bool,
) -> Result<(DataLayout, Source), GuardError> {
    let mut runs: Vec<(u64, Option<u64>, u64)> = Vec::new(); // (logical, physical, blocks)
    let source = if ino.flags & FLAG_INLINE != 0 {
        let n = usize::try_from(ino.size.min(60)).unwrap_or(0);
        let data = ino.block.get(..n).unwrap_or_default().to_vec();
        return Ok((
            DataLayout {
                extents: vec![Extent::Inline { data }],
            },
            Source::Inline,
        ));
    } else if ino.flags & FLAG_EXTENTS != 0 {
        let mut seen = HashSet::new();
        extents(
            r,
            g,
            &ino.block,
            MAX_DEPTH,
            stale,
            max_extents,
            &mut seen,
            &mut runs,
        )?;
        Source::Extents
    } else {
        block_map(r, g, &ino.block, max_extents, &mut runs)?;
        Source::BlockMap
    };
    runs.sort_by_key(|x| x.0);
    let bs = u64::from(g.block_size);
    let mut out = DataLayout::default();
    let mut pos = 0u64; // bytes
    for (logical, physical, count) in runs {
        let at = logical.saturating_mul(bs);
        if at >= ino.size {
            break;
        }
        if at > pos {
            out.extents.push(Extent::Zero { len: at - pos });
            pos = at;
        } else if at < pos {
            continue; // overlapping runs: keep the first
        }
        let len = count.saturating_mul(bs).min(ino.size - pos);
        match physical.and_then(|p| g.block_offset(p).filter(|_| p + count <= g.blocks)) {
            Some(off) => out.push_disk(off, len),
            None => out.extents.push(Extent::Zero { len }),
        }
        pos += len;
    }
    if pos < ino.size && source == Source::BlockMap {
        // A trailing hole.
        out.extents.push(Extent::Zero { len: ino.size - pos });
    }
    Ok((out, source))
}

#[allow(clippy::too_many_arguments, reason = "recursive walk sharing its state")]
fn extents(
    r: &GuardedReader,
    g: &Ext4Geometry,
    node: &[u8],
    depth_left: u16,
    stale: bool,
    max: u32,
    seen: &mut HashSet<u64>,
    runs: &mut Vec<(u64, Option<u64>, u64)>,
) -> Result<(), GuardError> {
    if u16_at(node, 0) != Some(EXTENT_MAGIC) {
        return Ok(());
    }
    let entries = u16_at(node, 2).unwrap_or(0);
    let cap = u16_at(node, 4).unwrap_or(0);
    let depth = u16_at(node, 6).unwrap_or(0);
    if depth >= depth_left || cap == 0 {
        return Ok(());
    }
    let room = u16::try_from((node.len().saturating_sub(12)) / 12).unwrap_or(u16::MAX);
    // A deleted inode has its entry count zeroed; its leaf entries may still be there.
    let n = if entries == 0 && stale && depth == 0 {
        cap.min(room)
    } else {
        entries.min(cap).min(room)
    };
    for i in 0..usize::from(n) {
        if runs.len() as u64 >= u64::from(max) {
            return Ok(());
        }
        let e = 12 + i * 12;
        let logical = u64::from(u32_at(node, e).unwrap_or(0));
        if depth == 0 {
            let raw_len = u16_at(node, e + 4).unwrap_or(0);
            let start = (u64::from(u16_at(node, e + 6).unwrap_or(0)) << 32)
                | u64::from(u32_at(node, e + 8).unwrap_or(0));
            if raw_len == 0 || (stale && entries == 0 && (start == 0 || start >= g.blocks)) {
                continue;
            }
            // Lengths over 32768 mark uninitialized extents: allocated but reading as zeros.
            let (len, init) = if raw_len > 32768 {
                (raw_len - 32768, false)
            } else {
                (raw_len, true)
            };
            runs.push((logical, init.then_some(start), u64::from(len)));
        } else {
            let leaf = (u64::from(u16_at(node, e + 8).unwrap_or(0)) << 32)
                | u64::from(u32_at(node, e + 4).unwrap_or(0));
            if !seen.insert(leaf) {
                continue;
            }
            let Some(off) = g.block_offset(leaf) else { continue };
            let mut child = vec![0u8; g.block_size as usize];
            match r.read(off, &mut child) {
                Ok(rep) if rep.is_clean() => {}
                Ok(_) | Err(GuardError::OutOfBounds) => continue,
                Err(e) => return Err(e),
            }
            extents(r, g, &child, depth, false, max, seen, runs)?;
        }
    }
    Ok(())
}

fn block_map(
    r: &GuardedReader,
    g: &Ext4Geometry,
    block: &[u8; 60],
    max: u32,
    runs: &mut Vec<(u64, Option<u64>, u64)>,
) -> Result<(), GuardError> {
    let per = u64::from(g.block_size / 4);
    let ptr = |i: usize| u64::from(u32_at(block, i * 4).unwrap_or(0));
    for i in 0..12 {
        push(runs, i as u64, ptr(i));
    }
    let mut budget = u64::from(max);
    indirect(r, g, ptr(12), 1, 12, per, &mut budget, runs)?;
    indirect(r, g, ptr(13), 2, 12 + per, per, &mut budget, runs)?;
    indirect(r, g, ptr(14), 3, 12 + per + per * per, per, &mut budget, runs)?;
    Ok(())
}

fn push(runs: &mut Vec<(u64, Option<u64>, u64)>, logical: u64, physical: u64) {
    if physical == 0 {
        return;
    }
    if let Some(last) = runs.last_mut()
        && last.0 + last.2 == logical
        && last.1.is_some_and(|p| p + last.2 == physical)
    {
        last.2 += 1;
        return;
    }
    runs.push((logical, Some(physical), 1));
}

#[allow(clippy::too_many_arguments, reason = "recursive walk sharing its state")]
fn indirect(
    r: &GuardedReader,
    g: &Ext4Geometry,
    at: u64,
    level: u32,
    first: u64,
    per: u64,
    budget: &mut u64,
    runs: &mut Vec<(u64, Option<u64>, u64)>,
) -> Result<(), GuardError> {
    if at == 0 || *budget == 0 {
        return Ok(());
    }
    *budget -= 1;
    let Some(off) = g.block_offset(at) else {
        return Ok(());
    };
    let mut b = vec![0u8; g.block_size as usize];
    match r.read(off, &mut b) {
        Ok(rep) if rep.is_clean() => {}
        Ok(_) | Err(GuardError::OutOfBounds) => return Ok(()),
        Err(e) => return Err(e),
    }
    let span = per.pow(level - 1);
    for i in 0..per {
        let p = u64::from(u32_at(&b, usize::try_from(i * 4).unwrap_or(0)).unwrap_or(0));
        if level == 1 {
            push(runs, first + i, p);
        } else {
            indirect(r, g, p, level - 1, first + i * span, per, budget, runs)?;
        }
    }
    Ok(())
}
