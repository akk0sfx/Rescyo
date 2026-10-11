//! MFT records: update-sequence fixups, attributes and runlists.
//!
//! Everything here is a pure function over bytes. A record that fails its fixup check was torn
//! or damaged and is rejected rather than half-trusted.

use rescyo_fs::{Times, Timestamp};
use rescyo_io::bytes::{i8_at, slice_at, u8_at, u16_at, u32_at, u64_at};

pub const ATTR_STANDARD_INFORMATION: u32 = 0x10;
pub const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
pub const ATTR_FILE_NAME: u32 = 0x30;
pub const ATTR_DATA: u32 = 0x80;
pub const ATTR_REPARSE_POINT: u32 = 0xC0;
/// Attribute flags.
pub const FLAG_COMPRESSED: u16 = 0x0001;
pub const FLAG_COMPRESSION_MASK: u16 = 0x00FF;
pub const FLAG_ENCRYPTED: u16 = 0x4000;
/// Reparse tag of a WOF-compressed file.
pub const IO_REPARSE_TAG_WOF: u32 = 0x8000_0017;
const ATTR_END: u32 = 0xFFFF_FFFF;
const MAX_ATTRS: usize = 256;

/// Applies the update sequence array in place. `sector` is the fixup stride (512 for every
/// NTFS version in the wild). Returns `false` if the record is torn or the array is malformed.
pub fn apply_fixup(rec: &mut [u8], sector: usize) -> bool {
    let (Some(usa_off), Some(usa_count)) = (u16_at(rec, 4), u16_at(rec, 6)) else {
        return false;
    };
    let (usa_off, usa_count) = (usize::from(usa_off), usize::from(usa_count));
    if usa_count < 2 || (usa_count - 1) * sector != rec.len() || usa_off + usa_count * 2 > rec.len() {
        return false;
    }
    let Some(usn) = u16_at(rec, usa_off) else {
        return false;
    };
    for i in 1..usa_count {
        let end = i * sector - 2;
        let Some(stored) = u16_at(rec, end) else {
            return false;
        };
        if stored != usn {
            return false;
        }
        let Some(real) = slice_at(rec, usa_off + i * 2, 2).and_then(|s| <[u8; 2]>::try_from(s).ok()) else {
            return false;
        };
        let Some(dst) = rec.get_mut(end..end + 2) else {
            return false;
        };
        dst.copy_from_slice(&real);
    }
    true
}

/// Header fields of a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub sequence: u16,
    pub in_use: bool,
    pub is_dir: bool,
    /// Base record reference (0 for a base record).
    pub base: u64,
    /// Record number stored in the record (NTFS 3.1+), if present.
    pub number: Option<u32>,
    pub attrs_offset: usize,
    pub used: usize,
}

/// Checks the signature and header of a record (after fixup).
pub fn header(rec: &[u8]) -> Option<Header> {
    if slice_at(rec, 0, 4)? != b"FILE" {
        return None;
    }
    let attrs_offset = usize::from(u16_at(rec, 20)?);
    let flags = u16_at(rec, 22)?;
    let used = usize::try_from(u32_at(rec, 24)?).ok()?;
    let alloc = usize::try_from(u32_at(rec, 28)?).ok()?;
    if attrs_offset < 0x30
        || attrs_offset >= rec.len()
        || used > rec.len()
        || alloc != rec.len()
        || used < attrs_offset
    {
        return None;
    }
    let number = (u16_at(rec, 4)? >= 0x30).then(|| u32_at(rec, 44)).flatten();
    Some(Header {
        sequence: u16_at(rec, 16)?,
        in_use: flags & 1 != 0,
        is_dir: flags & 2 != 0,
        base: u64_at(rec, 32)? & 0x0000_FFFF_FFFF_FFFF,
        number,
        attrs_offset,
        used,
    })
}

/// One attribute as found in a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr<'a> {
    pub kind: u32,
    pub name: String,
    pub flags: u16,
    pub body: AttrBody<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrBody<'a> {
    Resident(&'a [u8]),
    NonResident {
        start_vcn: u64,
        last_vcn: u64,
        runs: &'a [u8],
        alloc_size: u64,
        data_size: u64,
        init_size: u64,
        /// log2 of the clusters per compression unit (0 = not compressed).
        compression_unit: u8,
    },
}

/// The attributes of a record, stopping at the end marker or the first malformed one.
pub fn attributes<'a>(rec: &'a [u8], h: &Header) -> Vec<Attr<'a>> {
    let mut out = Vec::new();
    let mut o = h.attrs_offset;
    while out.len() < MAX_ATTRS {
        let Some(kind) = u32_at(rec, o) else { break };
        if kind == ATTR_END {
            break;
        }
        let Some(len) = u32_at(rec, o + 4).and_then(|l| usize::try_from(l).ok()) else {
            break;
        };
        if len < 16 || !len.is_multiple_of(8) || o + len > h.used {
            break;
        }
        let Some(a) = slice_at(rec, o, len) else { break };
        let non_res = u8_at(a, 8).unwrap_or(0) != 0;
        let name_len = usize::from(u8_at(a, 9).unwrap_or(0));
        let name_off = usize::from(u16_at(a, 10).unwrap_or(0));
        let flags = u16_at(a, 12).unwrap_or(0);
        let name = slice_at(a, name_off, name_len * 2)
            .map(|b| {
                let units: Vec<u16> = b
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect();
                String::from_utf16_lossy(&units)
            })
            .unwrap_or_default();
        let body = if non_res {
            let (Some(sv), Some(lv), Some(ro), Some(asz), Some(dsz), Some(isz)) = (
                u64_at(a, 16),
                u64_at(a, 24),
                u16_at(a, 32),
                u64_at(a, 40),
                u64_at(a, 48),
                u64_at(a, 56),
            ) else {
                break;
            };
            let Some(runs) = a.get(usize::from(ro)..) else {
                break;
            };
            AttrBody::NonResident {
                start_vcn: sv,
                last_vcn: lv,
                runs,
                alloc_size: asz,
                data_size: dsz,
                init_size: isz,
                compression_unit: u8_at(a, 34).unwrap_or(0),
            }
        } else {
            let (Some(vl), Some(vo)) = (u32_at(a, 16), u16_at(a, 20)) else {
                break;
            };
            let Some(v) = slice_at(a, usize::from(vo), usize::try_from(vl).unwrap_or(usize::MAX)) else {
                break;
            };
            AttrBody::Resident(v)
        };
        out.push(Attr {
            kind,
            name,
            flags,
            body,
        });
        o += len;
    }
    out
}

/// A decoded run: `lcn` is `None` for a sparse run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub lcn: Option<u64>,
    pub clusters: u64,
}

/// Decodes a runlist, rejecting negative LCNs and stopping at `max_runs` or `max_clusters`.
pub fn decode_runs(mut b: &[u8], max_runs: usize, max_clusters: u64) -> Option<Vec<Run>> {
    let mut out = Vec::new();
    let mut lcn: i64 = 0;
    let mut total: u64 = 0;
    while let Some(&head) = b.first() {
        if head == 0 {
            return Some(out);
        }
        let len_size = usize::from(head & 0x0F);
        let off_size = usize::from(head >> 4);
        if len_size == 0 || len_size > 8 || off_size > 8 || out.len() >= max_runs {
            return None;
        }
        let len_bytes = b.get(1..1 + len_size)?;
        let clusters = le_unsigned(len_bytes);
        if clusters == 0 {
            return None;
        }
        total = total.checked_add(clusters)?;
        if total > max_clusters {
            return None;
        }
        let run_lcn = if off_size == 0 {
            None
        } else {
            let off = le_signed(b.get(1 + len_size..1 + len_size + off_size)?);
            lcn = lcn.checked_add(off)?;
            Some(u64::try_from(lcn).ok()?)
        };
        out.push(Run {
            lcn: run_lcn,
            clusters,
        });
        b = b.get(1 + len_size + off_size..)?;
    }
    Some(out)
}

fn le_unsigned(b: &[u8]) -> u64 {
    b.iter().rev().fold(0u64, |v, &x| (v << 8) | u64::from(x))
}

fn le_signed(b: &[u8]) -> i64 {
    let v = le_unsigned(b);
    let bits = b.len() * 8;
    if bits == 0 || bits >= 64 {
        return v as i64;
    }
    let sign = 1u64 << (bits - 1);
    if v & sign != 0 {
        (v | !((1u64 << bits) - 1)) as i64
    } else {
        v as i64
    }
}

/// FILE_NAME attribute contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileName {
    pub parent: u64,
    pub namespace: u8,
    pub name: String,
}

pub fn file_name(v: &[u8]) -> Option<FileName> {
    let parent = u64_at(v, 0)?;
    let len = usize::from(u8_at(v, 64)?);
    let namespace = u8_at(v, 65)?;
    let units: Vec<u16> = slice_at(v, 66, len * 2)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let name = String::from_utf16(&units).ok()?;
    if name.is_empty() || name.contains(['/', '\0']) || namespace > 3 {
        return None;
    }
    Some(FileName {
        parent,
        namespace,
        name,
    })
}

/// Times from STANDARD_INFORMATION (UTC FILETIMEs).
pub fn standard_times(v: &[u8]) -> Times {
    let t = |o| u64_at(v, o).and_then(Timestamp::from_filetime);
    Times {
        created: t(0),
        modified: t(8),
        accessed: t(24),
    }
}

/// WOF compression from a reparse point's value: `Some(Ok(algorithm))` for a file-backed WOF
/// file, `Some(Err(provider))` for another provider (e.g. 1 = backed by a WIM file elsewhere).
pub fn wof_algorithm(reparse: &[u8]) -> Option<Result<u32, u32>> {
    if u32_at(reparse, 0)? != IO_REPARSE_TAG_WOF {
        return None;
    }
    let provider = u32_at(reparse, 12)?;
    if provider != 2 {
        return Some(Err(provider));
    }
    Some(Ok(u32_at(reparse, 20)?))
}

/// Bytes per MFT record from the boot sector's signed "clusters per record" field.
pub fn record_size(clusters_per_record: i8, cluster: u64) -> Option<u64> {
    if clusters_per_record > 0 {
        u64::try_from(clusters_per_record).ok()?.checked_mul(cluster)
    } else {
        let shift = u32::try_from(-i32::from(clusters_per_record)).ok()?;
        (shift < 32).then(|| 1u64 << shift)
    }
}

/// Reads the signed byte at `o` (helper for boot parsing).
pub fn signed_at(b: &[u8], o: usize) -> Option<i8> {
    i8_at(b, o)
}

#[cfg(test)]
mod tests;
