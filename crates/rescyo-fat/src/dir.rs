//! FAT12/16/32 directory entries: short names, long names, deleted entries and timestamps.
//!
//! All functions here are pure: they take bytes and return what they could validate. Entries
//! that fail validation are skipped, never guessed into existence.

use rescyo_fs::codepage::Codepage;
use rescyo_fs::{TimeZone, Times, Timestamp};
use rescyo_io::bytes::{slice_at, u8_at, u16_at, u32_at};

const ATTR_RO: u8 = 0x01;
const ATTR_VOLUME: u8 = 0x08;
const ATTR_DIR: u8 = 0x10;
const ATTR_LFN: u8 = 0x0F;
const DELETED: u8 = 0xE5;
const BAD_SHORT: &[u8] = b"\"*+,./:;<=>?[\\]|";

/// A validated directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    pub name: String,
    /// The 11 short-name bytes as stored (first byte 0xE5 for deleted entries).
    pub short: [u8; 11],
    pub has_long_name: bool,
    pub deleted: bool,
    pub is_dir: bool,
    pub read_only: bool,
    pub cluster: u32,
    pub size: u32,
    pub times: Times,
}

/// Context for validating entries of one volume.
#[derive(Debug, Clone, Copy)]
pub struct DirContext {
    pub fat32: bool,
    pub max_cluster: u32,
    /// Largest plausible file size (the data area).
    pub max_size: u64,
    pub codepage: Codepage,
}

/// Start cluster of the entry at `o` (the high word only exists on FAT32).
pub fn entry_cluster(d: &[u8], o: usize, fat32: bool) -> Option<u32> {
    let lo = u32::from(u16_at(d, o + 26)?);
    let hi = if fat32 { u32::from(u16_at(d, o + 20)?) } else { 0 };
    Some(hi << 16 | lo)
}

/// If `d` begins with `.` and `..` entries, returns (self cluster, parent cluster).
pub fn dir_start(d: &[u8], fat32: bool) -> Option<(u32, u32)> {
    let dot = slice_at(d, 0, 11)? == b".          " && u8_at(d, 11)? & ATTR_DIR != 0;
    let dotdot = slice_at(d, 32, 11)? == b"..         " && u8_at(d, 43)? & ATTR_DIR != 0;
    if !(dot && dotdot) {
        return None;
    }
    let me = entry_cluster(d, 0, fat32)?;
    let parent = entry_cluster(d, 32, fat32)?;
    (me >= 2 && me != parent).then_some((me, parent))
}

fn is_dot(d: &[u8], o: usize) -> bool {
    matches!(slice_at(d, o, 11), Some(b".          " | b"..         "))
}

/// Long-name entry with a plausible structure (sequence 1..=20, zero type and cluster fields).
fn valid_lfn(d: &[u8], o: usize) -> bool {
    let (Some(b0), Some(attr), Some(kind), Some(clus)) =
        (u8_at(d, o), u8_at(d, o + 11), u8_at(d, o + 12), u16_at(d, o + 26))
    else {
        return false;
    };
    let seq = b0 & 0x1F;
    attr & 0x3F == ATTR_LFN
        && kind == 0
        && clus == 0
        && (b0 == DELETED || ((1..=20).contains(&seq) && b0 & 0xA0 == 0))
}

fn valid_time(t: u16) -> bool {
    (t >> 11) <= 23 && ((t >> 5) & 63) <= 59 && (t & 31) <= 29
}

fn valid_date(d: u16) -> bool {
    d == 0 || ((1..=12).contains(&((d >> 5) & 15)) && d & 31 >= 1)
}

/// Short entry that could have been written by a real FAT driver.
fn valid_short(d: &[u8], o: usize, ctx: &DirContext) -> bool {
    let Some(attr) = u8_at(d, o + 11) else {
        return false;
    };
    if attr & 0xC0 != 0 || u8_at(d, o) == Some(0x20) {
        return false;
    }
    let Some(name) = slice_at(d, o, 11) else {
        return false;
    };
    for (i, &c) in name.iter().enumerate() {
        if i == 0 && (c == 0x05 || c == DELETED) {
            continue;
        }
        if c < 0x20 || BAD_SHORT.contains(&c) {
            return false;
        }
    }
    let (Some(t), Some(dt), Some(cdate), Some(ctime)) = (
        u16_at(d, o + 22),
        u16_at(d, o + 24),
        u16_at(d, o + 16),
        u16_at(d, o + 14),
    ) else {
        return false;
    };
    if !valid_time(t) || !valid_date(dt) || !valid_date(cdate) || !valid_time(ctime) {
        return false;
    }
    let (Some(c), Some(size)) = (entry_cluster(d, o, ctx.fat32), u32_at(d, o + 28)) else {
        return false;
    };
    let in_range = c >= 2 && c <= ctx.max_cluster;
    if attr & ATTR_DIR != 0 {
        return attr & ATTR_VOLUME == 0 && in_range && size == 0;
    }
    if attr & ATTR_VOLUME != 0 {
        return true;
    }
    if u64::from(size) > ctx.max_size {
        return false;
    }
    if size == 0 { c == 0 || in_range } else { in_range }
}

/// The checksum of a short name that its long-name entries carry.
pub fn short_checksum(name: &[u8; 11]) -> u8 {
    name.iter().fold(0u8, |sum, &b| {
        ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(b)
    })
}

/// For a deleted short name, finds the original first byte that makes the checksum match.
/// The checksum is a bijection of the first byte, so at most one value works.
fn recover_first_byte(short: &[u8; 11], checksum: u8) -> Option<u8> {
    (0x20u8..=0xFF).find(|&x| {
        let mut n = *short;
        n[0] = x;
        x != DELETED && !BAD_SHORT.contains(&x) && short_checksum(&n) == checksum
    })
}

fn fat_time(date: u16, time: u16, tenths: u8) -> Option<Timestamp> {
    if date == 0 {
        return None;
    }
    let tenths = u32::from(tenths.min(199));
    Timestamp::from_civil(
        1980 + i32::from(date >> 9),
        u32::from((date >> 5) & 15),
        u32::from(date & 31),
        u32::from(time >> 11),
        u32::from((time >> 5) & 63),
        u32::from(time & 31) * 2 + tenths / 100,
        (tenths % 100) * 10_000_000,
        TimeZone::Local,
    )
}

fn decode_short(short: &[u8; 11], nt_case: u8, cp: Codepage) -> String {
    let lower = |b: &[u8], on: bool| -> Vec<u8> {
        b.iter()
            .map(|&c| if on { c.to_ascii_lowercase() } else { c })
            .collect()
    };
    let (base, ext) = short.split_at(8);
    let trim = |b: &[u8]| -> Vec<u8> {
        let end = b.iter().rposition(|&c| c != b' ').map_or(0, |p| p + 1);
        b.get(..end).unwrap_or_default().to_vec()
    };
    let base = cp.decode(&lower(&trim(base), nt_case & 0x08 != 0));
    let ext = cp.decode(&lower(&trim(ext), nt_case & 0x10 != 0));
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

/// Long name being assembled from entries that precede a short entry.
#[derive(Debug, Default)]
struct LongName {
    parts: Vec<(u8, [u16; 13])>,
    checksum: u8,
    expect: u8,
    deleted: bool,
    ok: bool,
}

impl LongName {
    fn text(&self) -> Option<String> {
        let mut units = Vec::with_capacity(self.parts.len() * 13);
        let mut parts: Vec<&(u8, [u16; 13])> = self.parts.iter().collect();
        // Live parts carry their sequence number; deleted ones are in disk order (last part
        // first), so reverse them.
        if self.deleted {
            parts.reverse();
        } else {
            parts.sort_by_key(|p| p.0);
        }
        for (_, chunk) in parts {
            units.extend_from_slice(chunk);
        }
        let end = units
            .iter()
            .position(|&u| u == 0 || u == 0xFFFF)
            .unwrap_or(units.len());
        let text = String::from_utf16_lossy(units.get(..end)?);
        (!text.is_empty() && !text.contains('\u{FFFD}') && !text.contains(['/', '\\', '\0'])).then_some(text)
    }
}

fn lfn_chunk(d: &[u8], o: usize) -> [u16; 13] {
    let mut out = [0u16; 13];
    let offsets = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    for (slot, off) in out.iter_mut().zip(offsets) {
        *slot = u16_at(d, o + off).unwrap_or(0xFFFF);
    }
    out
}

/// Parses the 32-byte slots of a directory (any number of clusters, concatenated).
pub fn parse_dir(d: &[u8], ctx: &DirContext, max_entries: usize) -> Vec<RawEntry> {
    let mut out = Vec::new();
    let mut lfn = LongName::default();
    for o in (0..d.len().saturating_sub(31)).step_by(32) {
        if out.len() >= max_entries {
            break;
        }
        let Some(b0) = u8_at(d, o) else { break };
        let Some(attr) = u8_at(d, o + 11) else { break };
        if b0 == 0 {
            lfn = LongName::default();
            continue;
        }
        if attr & 0x3F == ATTR_LFN {
            if !valid_lfn(d, o) {
                lfn = LongName::default();
                continue;
            }
            let sum = u8_at(d, o + 13).unwrap_or(0);
            let chunk = lfn_chunk(d, o);
            if b0 == DELETED {
                if !(lfn.deleted && lfn.ok && lfn.checksum == sum) || lfn.parts.len() >= 20 {
                    lfn = LongName {
                        checksum: sum,
                        deleted: true,
                        ok: true,
                        ..LongName::default()
                    };
                }
                lfn.parts.push((0, chunk));
                continue;
            }
            let seq = b0 & 0x1F;
            if b0 & 0x40 != 0 {
                lfn = LongName {
                    checksum: sum,
                    expect: seq,
                    ok: true,
                    ..LongName::default()
                };
            }
            if lfn.ok && !lfn.deleted && seq == lfn.expect && sum == lfn.checksum {
                lfn.parts.push((seq, chunk));
                lfn.expect -= 1;
            } else {
                lfn = LongName::default();
            }
            continue;
        }
        let pending = std::mem::take(&mut lfn);
        if b0 == 0x20 || is_dot(d, o) || !valid_short(d, o, ctx) || attr & ATTR_VOLUME != 0 {
            continue;
        }
        let Some(mut short) = slice_at(d, o, 11).and_then(|s| <[u8; 11]>::try_from(s).ok()) else {
            continue;
        };
        let deleted = b0 == DELETED;
        let mut name = None;
        if deleted {
            if pending.deleted
                && pending.ok
                && let Some(first) = recover_first_byte(&short, pending.checksum)
            {
                name = pending.text();
                if name.is_some() {
                    short[0] = first;
                }
            }
        } else {
            if short[0] == 0x05 {
                short[0] = DELETED;
            }
            if pending.ok
                && !pending.deleted
                && pending.expect == 0
                && short_checksum(&short) == pending.checksum
            {
                name = pending.text();
            }
        }
        let has_long_name = name.is_some();
        let name = name.unwrap_or_else(|| {
            let nt = u8_at(d, o + 12).unwrap_or(0);
            if deleted {
                let mut s = short;
                s[0] = b'_';
                decode_short(&s, nt, ctx.codepage)
            } else {
                decode_short(&short, nt, ctx.codepage)
            }
        });
        let u16o = |off| u16_at(d, o + off).unwrap_or(0);
        let times = Times {
            created: fat_time(u16o(16), u16o(14), u8_at(d, o + 13).unwrap_or(0)),
            modified: fat_time(u16o(24), u16o(22), 0),
            accessed: fat_time(u16o(18), 0, 0),
        };
        let is_dir = attr & ATTR_DIR != 0;
        out.push(RawEntry {
            name,
            short,
            has_long_name,
            deleted,
            is_dir,
            read_only: attr & ATTR_RO != 0,
            cluster: entry_cluster(d, o, ctx.fat32).unwrap_or(0),
            size: if is_dir { 0 } else { u32_at(d, o + 28).unwrap_or(0) },
            times,
        });
    }
    out
}

/// Scores how directory-like a cluster is: (valid slots, invalid slots).
pub fn score(d: &[u8], ctx: &DirContext) -> (u32, u32) {
    let (mut valid, mut invalid) = (0, 0);
    for o in (0..d.len().saturating_sub(31)).step_by(32) {
        let Some(slot) = slice_at(d, o, 32) else { break };
        match slot.first() {
            Some(0) => {
                if slot.iter().any(|&b| b != 0) {
                    invalid += 1;
                }
            }
            Some(&DELETED) => {}
            _ => {
                if valid_lfn(d, o) || is_dot(d, o) || valid_short(d, o, ctx) {
                    valid += 1;
                } else {
                    invalid += 1;
                }
            }
        }
    }
    (valid, invalid)
}

/// Whether `d` looks like part of a directory (at most one bad slot per two good ones).
pub fn looks_like_dir(d: &[u8], ctx: &DirContext, require_entries: bool) -> bool {
    let (v, i) = score(d, ctx);
    !(require_entries && v == 0) && i * 2 <= v
}

#[cfg(test)]
mod tests;
