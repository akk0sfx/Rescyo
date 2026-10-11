//! GNU ddrescue mapfiles: which parts of a Source an image holds.
//!
//! Compatible both ways: a map written by Rescyo can be continued by `ddrescue`, and a map
//! written by `ddrescue` tells Rescyo which parts of an image are zeros instead of data.
//!
//! ```text
//! # current_pos  current_status  current_pass
//! 0x00120000     ?               1
//! #      pos        size  status
//! 0x00000000  0x00117000  +
//! 0x00117000  0x00000200  -
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::Range;

use crate::BadSectorMap;

/// Most block lines accepted when parsing (a hostile file must not exhaust memory).
const MAX_BLOCKS: usize = 10_000_000;

/// The status of a block, with ddrescue's characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockStatus {
    /// `?` not tried yet.
    NonTried,
    /// `*` failed during copying, edges not trimmed yet.
    NonTrimmed,
    /// `/` trimmed, the inside not scraped sector by sector yet.
    NonScraped,
    /// `-` a bad sector: read and failed.
    Bad,
    /// `+` rescued.
    Finished,
}

impl BlockStatus {
    pub fn as_char(self) -> char {
        match self {
            Self::NonTried => '?',
            Self::NonTrimmed => '*',
            Self::NonScraped => '/',
            Self::Bad => '-',
            Self::Finished => '+',
        }
    }

    pub fn from_char(c: char) -> Option<Self> {
        Some(match c {
            '?' => Self::NonTried,
            '*' => Self::NonTrimmed,
            '/' => Self::NonScraped,
            '-' => Self::Bad,
            '+' => Self::Finished,
            _ => return None,
        })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MapfileError {
    #[error("line {line}: {reason}")]
    Syntax { line: usize, reason: &'static str },
    #[error("too many blocks")]
    TooLarge,
    #[error("blocks overlap or are out of order at line {0}")]
    Order(usize),
}

/// A mapfile: status of every byte range of a Source, plus where the copy stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapfile {
    pub current_pos: u64,
    /// The phase the copy is in (ddrescue's `current_status` character).
    pub current_status: char,
    pub current_pass: u32,
    /// start -> (end, status); contiguous, non-overlapping, adjacent equal statuses merged.
    blocks: BTreeMap<u64, (u64, BlockStatus)>,
    size: u64,
}

impl Mapfile {
    /// A fresh map: everything not tried.
    pub fn new(size: u64) -> Self {
        let mut blocks = BTreeMap::new();
        if size > 0 {
            blocks.insert(0, (size, BlockStatus::NonTried));
        }
        Self {
            current_pos: 0,
            current_status: '?',
            current_pass: 1,
            blocks,
            size,
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Parses a ddrescue mapfile. Areas the file does not mention are not tried.
    pub fn parse(text: &str) -> Result<Self, MapfileError> {
        let mut status_line: Option<(u64, char, u32)> = None;
        let mut entries: Vec<(u64, u64, BlockStatus)> = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            let err = |reason| MapfileError::Syntax { line: i + 1, reason };
            if status_line.is_none() {
                let pos = f
                    .first()
                    .and_then(|x| num(x))
                    .ok_or(err("bad current position"))?;
                let st = f
                    .get(1)
                    .and_then(|x| x.chars().next())
                    .ok_or(err("missing current status"))?;
                let pass = f.get(2).and_then(|x| x.parse().ok()).unwrap_or(1);
                status_line = Some((pos, st, pass));
                continue;
            }
            if entries.len() >= MAX_BLOCKS {
                return Err(MapfileError::TooLarge);
            }
            let pos = f.first().and_then(|x| num(x)).ok_or(err("bad position"))?;
            let size = f.get(1).and_then(|x| num(x)).ok_or(err("bad size"))?;
            let st = f
                .get(2)
                .and_then(|x| x.chars().next())
                .and_then(BlockStatus::from_char)
                .ok_or(err("bad status"))?;
            if size == 0 {
                continue;
            }
            if let Some(&(p, s, _)) = entries.last()
                && pos < p + s
            {
                return Err(MapfileError::Order(i + 1));
            }
            pos.checked_add(size)
                .ok_or(err("block past the end of addressable space"))?;
            entries.push((pos, size, st));
        }
        let (current_pos, current_status, current_pass) = status_line.unwrap_or((0, '?', 1));
        let size = entries.last().map_or(0, |&(p, s, _)| p + s);
        let mut m = Self::new(size);
        (m.current_pos, m.current_status, m.current_pass) = (current_pos, current_status, current_pass);
        for (p, s, st) in entries {
            m.set(p..p + s, st);
        }
        Ok(m)
    }

    /// Grows or shrinks the map to `size` (a map from a larger or smaller copy).
    pub fn resize(&mut self, size: u64) {
        if size > self.size {
            let start = self.size;
            self.size = size;
            self.set(start..size, BlockStatus::NonTried);
        } else {
            let cut: Vec<u64> = self.blocks.range(size..).map(|(&s, _)| s).collect();
            for s in cut {
                self.blocks.remove(&s);
            }
            if let Some((_, (e, _))) = self.blocks.range_mut(..size).next_back()
                && *e > size
            {
                *e = size;
            }
            self.size = size;
        }
    }

    /// Sets the status of `range` (clipped to the map), splitting and merging blocks.
    pub fn set(&mut self, range: Range<u64>, status: BlockStatus) {
        let (start, end) = (range.start.min(self.size), range.end.min(self.size));
        if start >= end {
            return;
        }
        // Split blocks at both edges.
        for at in [start, end] {
            if let Some((&s, &(e, st))) = self.blocks.range(..at).next_back()
                && e > at
            {
                self.blocks.insert(s, (at, st));
                self.blocks.insert(at, (e, st));
            }
        }
        let inside: Vec<u64> = self.blocks.range(start..end).map(|(&s, _)| s).collect();
        for s in inside {
            self.blocks.remove(&s);
        }
        self.blocks.insert(start, (end, status));
        self.merge_around(start);
    }

    fn merge_around(&mut self, start: u64) {
        let Some(&(mut end, st)) = self.blocks.get(&start) else {
            return;
        };
        let mut start = start;
        if let Some((&ps, &(pe, pst))) = self.blocks.range(..start).next_back()
            && pe == start
            && pst == st
        {
            self.blocks.remove(&start);
            start = ps;
        }
        if let Some(&(ne, nst)) = self.blocks.get(&end)
            && nst == st
        {
            self.blocks.remove(&end);
            end = ne;
        }
        self.blocks.insert(start, (end, st));
    }

    /// The status of the byte at `pos`.
    pub fn status_at(&self, pos: u64) -> Option<BlockStatus> {
        self.blocks
            .range(..=pos)
            .next_back()
            .filter(|(_, (e, _))| pos < *e)
            .map(|(_, (_, st))| *st)
    }

    /// All blocks in order.
    pub fn blocks(&self) -> impl Iterator<Item = (Range<u64>, BlockStatus)> + '_ {
        self.blocks.iter().map(|(&s, &(e, st))| (s..e, st))
    }

    /// Ranges with the given status, in order.
    pub fn ranges(&self, status: BlockStatus) -> Vec<Range<u64>> {
        self.blocks()
            .filter(|(_, st)| *st == status)
            .map(|(r, _)| r)
            .collect()
    }

    /// Bytes with the given status.
    pub fn bytes(&self, status: BlockStatus) -> u64 {
        self.blocks()
            .filter(|(_, st)| *st == status)
            .map(|(r, _)| r.end - r.start)
            .sum()
    }

    /// Sectors whose data the image does not hold (anything not rescued): they read as zeros
    /// in the image and must be reported as unreadable.
    pub fn bad_map(&self, sector: u32) -> BadSectorMap {
        let ss = u64::from(sector.max(1));
        self.blocks()
            .filter(|(_, st)| *st != BlockStatus::Finished)
            .map(|(r, _)| r.start / ss..r.end.div_ceil(ss))
            .collect()
    }

    /// ddrescue mapfile text.
    pub fn to_text(&self, tool: &str) -> String {
        let mut s = format!("# Mapfile. Created by {tool}\n# current_pos  current_status  current_pass\n");
        let _ = writeln!(
            s,
            "0x{:08X}     {}               {}",
            self.current_pos, self.current_status, self.current_pass
        );
        s.push_str("#      pos        size  status\n");
        for (r, st) in self.blocks() {
            let _ = writeln!(
                s,
                "0x{:08X}  0x{:08X}  {}",
                r.start,
                r.end - r.start,
                st.as_char()
            );
        }
        s
    }
}

fn num(s: &str) -> Option<u64> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written by GNU ddrescue 1.27.
    const DDRESCUE: &str = "# Mapfile. Created by GNU ddrescue version 1.27\n\
# Command line: ddrescue -d /dev/sdb disk.img disk.map\n\
# Start time:   2026-10-11 10:00:00\n\
# current_pos  current_status  current_pass\n\
0x00120000     ?               1\n\
#      pos        size  status\n\
0x00000000  0x00117000  +\n\
0x00117000  0x00000200  -\n\
0x00117200  0x00008E00  /\n\
0x00120000  0x0FEE0000  ?\n";

    #[test]
    fn parses_ddrescue_maps_and_writes_them_back() {
        let m = Mapfile::parse(DDRESCUE).unwrap();
        assert_eq!(m.size(), 0x1000_0000);
        assert_eq!(
            (m.current_pos, m.current_status, m.current_pass),
            (0x12_0000, '?', 1)
        );
        assert_eq!(m.status_at(0x11_7100), Some(BlockStatus::Bad));
        assert_eq!(m.bytes(BlockStatus::Finished), 0x11_7000);
        let again = Mapfile::parse(&m.to_text("Rescyo")).unwrap();
        assert_eq!(again, m);
    }

    #[test]
    fn set_splits_and_merges() {
        let mut m = Mapfile::new(1000);
        m.set(100..200, BlockStatus::Finished);
        m.set(200..300, BlockStatus::Finished);
        assert_eq!(m.ranges(BlockStatus::Finished), vec![100..300]);
        m.set(150..160, BlockStatus::Bad);
        assert_eq!(m.blocks().count(), 5);
        m.set(150..160, BlockStatus::Finished);
        assert_eq!(m.ranges(BlockStatus::Finished), vec![100..300]);
        m.set(0..1000, BlockStatus::Finished);
        assert_eq!(m.blocks().count(), 1);
        m.set(900..2000, BlockStatus::Bad);
        assert_eq!(m.ranges(BlockStatus::Bad), vec![900..1000]);
    }

    #[test]
    fn everything_not_rescued_is_bad_in_the_image() {
        let m = Mapfile::parse(DDRESCUE).unwrap();
        let bad = m.bad_map(512);
        assert!(!bad.is_bad(0));
        assert!(bad.is_bad(0x11_7000 / 512));
        assert!(bad.is_bad(0x12_0000 / 512));
        assert_eq!(bad.bad_sectors(), (0x1000_0000 - 0x11_7000) / 512);
    }

    #[test]
    fn hostile_maps_are_rejected() {
        assert!(Mapfile::parse("0 ? 1\n0x10 0x10 +\n0x0 0x10 +\n").is_err());
        assert!(Mapfile::parse("0 ? 1\n0 10 X\n").is_err());
        assert!(Mapfile::parse("0 ? 1\n0xFFFFFFFFFFFFFFFF 0x10 +\n").is_err());
        assert_eq!(Mapfile::parse("").unwrap().size(), 0);
    }

    #[test]
    fn resize() {
        let mut m = Mapfile::new(100);
        m.set(0..100, BlockStatus::Finished);
        m.resize(150);
        assert_eq!(m.ranges(BlockStatus::NonTried), vec![100..150]);
        m.resize(50);
        assert_eq!(
            m.blocks().collect::<Vec<_>>(),
            vec![(0..50, BlockStatus::Finished)]
        );
    }
}
