//! exFAT directory entry sets.
//!
//! A file is a set of 32-byte entries: File (0x85), Stream Extension (0xC0) and File Name
//! (0xC1) entries, protected by a checksum over the whole set. Deleted sets keep their content
//! with the InUse bit (0x80) cleared in every entry type, so they are verified by restoring it.

use rescyo_fs::{TimeZone, Times, Timestamp};
use rescyo_io::bytes::{slice_at, u8_at, u16_at, u32_at, u64_at};

const FILE: u8 = 0x05;
const STREAM: u8 = 0x40;
const NAME: u8 = 0x41;
const IN_USE: u8 = 0x80;

/// A validated exFAT file or directory entry set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExEntry {
    pub name: String,
    pub deleted: bool,
    pub is_dir: bool,
    pub read_only: bool,
    pub first_cluster: u32,
    pub data_len: u64,
    pub valid_len: u64,
    /// The allocation is contiguous and the FAT is not used for it.
    pub no_fat_chain: bool,
    pub times: Times,
}

/// System entries of a root directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootInfo {
    pub bitmap: Option<(u32, u64)>,
    pub upcase: Option<(u32, u64)>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct ExContext {
    pub max_cluster: u32,
    pub cluster_size: u32,
    pub max_size: u64,
}

/// Checksum over an entry set, skipping the checksum field itself. `restore_in_use` sets the
/// InUse bit of every entry type first, as it was when the set was written.
pub fn set_checksum(set: &[u8], restore_in_use: bool) -> u16 {
    let mut sum: u16 = 0;
    for (i, &b) in set.iter().enumerate() {
        if i == 2 || i == 3 {
            continue;
        }
        let b = if restore_in_use && i % 32 == 0 {
            b | IN_USE
        } else {
            b
        };
        sum = sum.rotate_right(1).wrapping_add(u16::from(b));
    }
    sum
}

fn ts(raw: u32, ten_ms: u8, utc: u8) -> Option<Timestamp> {
    if raw == 0 {
        return None;
    }
    let ten_ms = u32::from(ten_ms.min(199));
    let mut t = Timestamp::from_civil(
        1980 + i32::try_from(raw >> 25).ok()?,
        (raw >> 21) & 0xF,
        (raw >> 16) & 0x1F,
        (raw >> 11) & 0x1F,
        (raw >> 5) & 0x3F,
        (raw & 0x1F) * 2 + ten_ms / 100,
        (ten_ms % 100) * 10_000_000,
        TimeZone::Local,
    )?;
    if utc & 0x80 != 0 {
        // Signed 7-bit count of 15-minute steps east of UTC.
        let steps = i64::from(((utc & 0x7F) << 1) as i8 >> 1);
        t.unix -= steps * 15 * 60;
        t.zone = TimeZone::Utc;
    }
    Some(t)
}

/// Tries to read an entry set starting at slot offset `o`. Returns the entry and the number of
/// slots it spans.
fn file_set(d: &[u8], o: usize, ctx: &ExContext) -> Option<(ExEntry, usize)> {
    let t = u8_at(d, o)?;
    let deleted = t & IN_USE == 0;
    if t & !IN_USE != FILE {
        return None;
    }
    let secondaries = usize::from(u8_at(d, o + 1)?);
    if !(2..=18).contains(&secondaries) {
        return None;
    }
    let set = slice_at(d, o, 32 * (secondaries + 1))?;
    let want_type = |off: usize, base: u8| -> bool {
        u8_at(set, off).is_some_and(|x| x == if deleted { base } else { base | IN_USE })
    };
    if !want_type(32, STREAM) {
        return None;
    }
    let stored = u16_at(set, 2)?;
    if set_checksum(set, deleted) != stored {
        return None;
    }
    let attrs = u16_at(set, 4)?;
    let is_dir = attrs & 0x10 != 0;
    let flags = u8_at(set, 33)?;
    let name_len = usize::from(u8_at(set, 35)?);
    let valid_len = u64_at(set, 40)?;
    let first_cluster = u32_at(set, 52)?;
    let data_len = u64_at(set, 56)?;
    let name_entries = name_len.div_ceil(15);
    if name_len == 0 || name_entries + 1 > secondaries {
        return None;
    }
    let mut units = Vec::with_capacity(name_len);
    for k in 0..name_entries {
        let off = 64 + k * 32;
        if !want_type(off, NAME) {
            return None;
        }
        for c in 0..15 {
            if units.len() == name_len {
                break;
            }
            units.push(u16_at(set, off + 2 + c * 2)?);
        }
    }
    let name = String::from_utf16(&units).ok()?;
    if name.contains(['/', '\\', '\0']) || name == "." || name == ".." {
        return None;
    }
    let in_range = first_cluster >= 2 && first_cluster <= ctx.max_cluster;
    // Empty files may or may not keep AllocationPossible set, depending on the driver.
    let cluster_ok = if data_len == 0 {
        first_cluster == 0 || in_range
    } else {
        flags & 1 != 0 && in_range
    };
    if !cluster_ok || valid_len > data_len || data_len > ctx.max_size {
        return None;
    }
    if is_dir && !data_len.is_multiple_of(u64::from(ctx.cluster_size)) {
        return None;
    }
    let times = Times {
        created: ts(u32_at(set, 8)?, u8_at(set, 20)?, u8_at(set, 22)?),
        modified: ts(u32_at(set, 12)?, u8_at(set, 21)?, u8_at(set, 23)?),
        accessed: ts(u32_at(set, 16)?, 0, u8_at(set, 24)?),
    };
    Some((
        ExEntry {
            name,
            deleted,
            is_dir,
            read_only: attrs & 0x01 != 0,
            first_cluster,
            data_len,
            valid_len,
            no_fat_chain: flags & 2 != 0,
            times,
        },
        secondaries + 1,
    ))
}

/// Parses a directory's bytes into entries and, for a root directory, its system entries.
pub fn parse_exdir(d: &[u8], ctx: &ExContext, max_entries: usize) -> (Vec<ExEntry>, RootInfo) {
    let mut out = Vec::new();
    let mut info = RootInfo::default();
    let mut o = 0;
    while o + 32 <= d.len() && out.len() < max_entries {
        let t = u8_at(d, o).unwrap_or(0);
        if let Some((e, slots)) = file_set(d, o, ctx) {
            out.push(e);
            o += slots * 32;
            continue;
        }
        match t {
            0x81 => {
                if let (Some(c), Some(len)) = (u32_at(d, o + 20), u64_at(d, o + 24)) {
                    info.bitmap.get_or_insert((c, len));
                }
            }
            0x82 => {
                if let (Some(c), Some(len)) = (u32_at(d, o + 20), u64_at(d, o + 24)) {
                    info.upcase = Some((c, len));
                }
            }
            0x83 => {
                let n = usize::from(u8_at(d, o + 1).unwrap_or(0)).min(11);
                let units: Vec<u16> = (0..n).filter_map(|i| u16_at(d, o + 2 + i * 2)).collect();
                info.label = String::from_utf16(&units).ok().filter(|s| !s.is_empty());
            }
            _ => {}
        }
        o += 32;
    }
    (out, info)
}

/// Whether `d` looks like part of an exFAT directory: valid entry sets or system entries, and
/// little else that is non-zero.
pub fn looks_like_exdir(d: &[u8], ctx: &ExContext, require_entries: bool) -> bool {
    let (mut good, mut bad) = (0usize, 0usize);
    let mut o = 0;
    while o + 32 <= d.len() {
        if let Some((_, slots)) = file_set(d, o, ctx) {
            good += slots;
            o += slots * 32;
            continue;
        }
        match u8_at(d, o).unwrap_or(0) {
            0 => {
                if slice_at(d, o, 32).is_some_and(|s| s.iter().any(|&b| b != 0)) {
                    bad += 1;
                }
            }
            0x81 | 0x82 | 0x83 | 0xA0 | 0xA1 | 0xA2 | 0x03 => good += 1,
            // Deleted fragments of sets whose start was in a previous cluster.
            0x40 | 0x41 | 0xC0 | 0xC1 => {}
            _ => bad += 1,
        }
        o += 32;
    }
    !(require_entries && good == 0) && bad * 2 <= good
}

#[cfg(test)]
mod tests;
