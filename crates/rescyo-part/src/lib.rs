//! Partition table readers (MBR with EBR chains, GPT with backup fallback).
//!
//! Only intact tables are read here. Searching for lost partitions is a later stage; volume
//! candidates without any table entry are found by the file-system probes.
#![forbid(unsafe_code)]

use std::collections::HashSet;

use rescyo_io::GuardedReader;
use rescyo_io::bytes::{bytes_at, slice_at, u8_at, u16_at, u32_at, u64_at};

pub use rescyo_io::GuardError;

/// Most logical partitions followed in an EBR chain.
const MAX_LOGICAL: usize = 128;
/// Upper bound for a GPT entry array (the spec minimum is 16 KiB).
const MAX_GPT_ARRAY: u64 = 1 << 20;

/// Which kind of table was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Mbr,
    Gpt,
    /// No partition table: the Source may be a superfloppy volume or damaged.
    None,
}

/// What a partition entry says about its contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartitionType {
    Mbr(u8),
    Gpt {
        type_guid: [u8; 16],
        unique_guid: [u8; 16],
    },
}

/// One usable partition. Offsets are in bytes from the start of the Source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// 1-based position: primary 1–4 and logical 5+ for MBR; entry index + 1 for GPT.
    pub number: u32,
    pub offset: u64,
    pub size: u64,
    pub kind: PartitionType,
    pub name: Option<String>,
}

/// Things worth telling the user about a table that was still usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableWarning {
    PrimaryGptInvalid,
    BackupGptInvalid,
    EntryBeyondSource { number: u32 },
    EbrChainBroken,
    Overlap { a: u32, b: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionTable {
    pub kind: TableKind,
    pub partitions: Vec<Partition>,
    pub warnings: Vec<TableWarning>,
}

/// Reads the partition table of a Source. A GPT is tried even when the protective MBR is gone.
pub fn read_partitions(r: &GuardedReader) -> Result<PartitionTable, GuardError> {
    let g = r.geometry();
    let ss = u64::from(g.logical_sector);
    let sectors = g.sectors();
    let mut warnings = Vec::new();

    if sectors >= 3 {
        let primary = read_gpt(r, 1, ss, sectors)?;
        let backup = read_gpt(r, sectors - 1, ss, sectors)?;
        let chosen = match (primary, backup) {
            (Some(p), b) => {
                if b.is_none() {
                    warnings.push(TableWarning::BackupGptInvalid);
                }
                Some(p)
            }
            (None, Some(b)) => {
                warnings.push(TableWarning::PrimaryGptInvalid);
                Some(b)
            }
            (None, None) => None,
        };
        if let Some(parts) = chosen {
            let partitions = finish(parts, g.size, &mut warnings);
            return Ok(PartitionTable {
                kind: TableKind::Gpt,
                partitions,
                warnings,
            });
        }
    }

    let mut lba0 = vec![0u8; usize::try_from(ss).map_err(|_| GuardError::OutOfBounds)?];
    if g.size < ss || !r.read(0, &mut lba0)?.is_clean() {
        return Ok(PartitionTable {
            kind: TableKind::None,
            partitions: Vec::new(),
            warnings,
        });
    }
    let Some(entries) = parse_mbr(&lba0, sectors) else {
        return Ok(PartitionTable {
            kind: TableKind::None,
            partitions: Vec::new(),
            warnings,
        });
    };
    let mut parts = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let number = u32::try_from(i).unwrap_or(0) + 1;
        if is_extended(e.kind) {
            read_ebr_chain(r, e.start, ss, sectors, &mut parts, &mut warnings)?;
        } else if e.kind != 0 {
            parts.push(Partition {
                number,
                offset: e.start.saturating_mul(ss),
                size: e.len.saturating_mul(ss),
                kind: PartitionType::Mbr(e.kind),
                name: None,
            });
        }
    }
    let partitions = finish(parts, g.size, &mut warnings);
    Ok(PartitionTable {
        kind: TableKind::Mbr,
        partitions,
        warnings,
    })
}

/// A raw MBR partition entry, in sectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MbrEntry {
    pub status: u8,
    pub kind: u8,
    pub start: u64,
    pub len: u64,
}

/// Parses a sector as an MBR. `None` if it is not one: no 0x55AA, an invalid status byte, or
/// a boot sector of a file system (a superfloppy) whose "entries" are really code and BPB.
pub fn parse_mbr(sector: &[u8], total_sectors: u64) -> Option<[MbrEntry; 4]> {
    if u16_at(sector, 510)? != 0xAA55 {
        return None;
    }
    let mut entries = [MbrEntry {
        status: 0,
        kind: 0,
        start: 0,
        len: 0,
    }; 4];
    let mut used = 0;
    for (i, slot) in entries.iter_mut().enumerate() {
        let o = 446 + i * 16;
        let status = u8_at(sector, o)?;
        if status != 0 && status != 0x80 {
            return None;
        }
        let kind = u8_at(sector, o + 4)?;
        let start = u64::from(u32_at(sector, o + 8)?);
        let len = u64::from(u32_at(sector, o + 12)?);
        if kind != 0 {
            if start == 0 || len == 0 {
                return None;
            }
            // Allow tables made for a slightly larger disk (images trimmed by a sector or two).
            if start >= total_sectors {
                return None;
            }
            used += 1;
        }
        *slot = MbrEntry {
            status,
            kind,
            start,
            len,
        };
    }
    if used == 0 {
        return None;
    }
    if looks_like_volume_boot_sector(sector) && entries.iter().any(|e| e.kind != 0 && e.start < 2) {
        return None;
    }
    Some(entries)
}

fn looks_like_volume_boot_sector(s: &[u8]) -> bool {
    let jump = matches!(u8_at(s, 0), Some(0xEB | 0xE9));
    let oem = slice_at(s, 3, 8);
    let fat = slice_at(s, 54, 3) == Some(b"FAT") || slice_at(s, 82, 3) == Some(b"FAT");
    jump && (fat || oem == Some(b"NTFS    ") || oem == Some(b"EXFAT   "))
}

fn is_extended(kind: u8) -> bool {
    matches!(kind, 0x05 | 0x0F | 0x85)
}

fn read_sector(r: &GuardedReader, lba: u64, ss: u64) -> Result<Option<Vec<u8>>, GuardError> {
    let mut buf = vec![0u8; usize::try_from(ss).map_err(|_| GuardError::OutOfBounds)?];
    let Some(off) = lba.checked_mul(ss) else {
        return Ok(None);
    };
    match r.read(off, &mut buf) {
        Ok(rep) if rep.is_clean() => Ok(Some(buf)),
        Ok(_) | Err(GuardError::OutOfBounds) => Ok(None),
        Err(e) => Err(e),
    }
}

fn read_ebr_chain(
    r: &GuardedReader,
    ext_start: u64,
    ss: u64,
    sectors: u64,
    parts: &mut Vec<Partition>,
    warnings: &mut Vec<TableWarning>,
) -> Result<(), GuardError> {
    let mut seen = HashSet::new();
    let mut ebr = ext_start;
    let mut number = 5u32;
    for _ in 0..MAX_LOGICAL {
        if !seen.insert(ebr) || ebr >= sectors {
            warnings.push(TableWarning::EbrChainBroken);
            return Ok(());
        }
        let Some(sector) = read_sector(r, ebr, ss)? else {
            warnings.push(TableWarning::EbrChainBroken);
            return Ok(());
        };
        if u16_at(&sector, 510) != Some(0xAA55) {
            warnings.push(TableWarning::EbrChainBroken);
            return Ok(());
        }
        let entry = |i: usize| -> Option<(u8, u64, u64)> {
            let o = 446 + i * 16;
            Some((
                u8_at(&sector, o + 4)?,
                u64::from(u32_at(&sector, o + 8)?),
                u64::from(u32_at(&sector, o + 12)?),
            ))
        };
        if let Some((kind, rel, len)) = entry(0)
            && kind != 0
            && len != 0
        {
            parts.push(Partition {
                number,
                offset: ebr.saturating_add(rel).saturating_mul(ss),
                size: len.saturating_mul(ss),
                kind: PartitionType::Mbr(kind),
                name: None,
            });
            number += 1;
        }
        match entry(1) {
            Some((kind, rel, _)) if is_extended(kind) && rel != 0 => ebr = ext_start.saturating_add(rel),
            _ => return Ok(()),
        }
    }
    warnings.push(TableWarning::EbrChainBroken);
    Ok(())
}

/// Reads and validates a GPT header at `lba` and its entry array.
fn read_gpt(
    r: &GuardedReader,
    lba: u64,
    ss: u64,
    sectors: u64,
) -> Result<Option<Vec<Partition>>, GuardError> {
    let Some(header) = read_sector(r, lba, ss)? else {
        return Ok(None);
    };
    let Some(h) = parse_gpt_header(&header, lba, sectors) else {
        return Ok(None);
    };
    let array_len = u64::from(h.entry_count) * u64::from(h.entry_size);
    let Some(off) = h.entries_lba.checked_mul(ss) else {
        return Ok(None);
    };
    let mut array = vec![0u8; usize::try_from(array_len).map_err(|_| GuardError::OutOfBounds)?];
    match r.read(off, &mut array) {
        Ok(rep) if rep.is_clean() => {}
        Ok(_) | Err(GuardError::OutOfBounds) => return Ok(None),
        Err(e) => return Err(e),
    }
    if crc32fast::hash(&array) != h.entries_crc {
        return Ok(None);
    }
    Ok(Some(parse_gpt_entries(&array, &h, ss)))
}

/// The fields of a GPT header that matter here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GptHeader {
    pub my_lba: u64,
    pub first_usable: u64,
    pub last_usable: u64,
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
    pub entries_crc: u32,
}

/// Validates a GPT header sector: signature, size, CRC, self-reference and sane array bounds.
pub fn parse_gpt_header(s: &[u8], expected_lba: u64, total_sectors: u64) -> Option<GptHeader> {
    if slice_at(s, 0, 8)? != b"EFI PART" {
        return None;
    }
    let header_size = usize::try_from(u32_at(s, 12)?).ok()?;
    if !(92..=s.len()).contains(&header_size) {
        return None;
    }
    let stored_crc = u32_at(s, 16)?;
    let mut copy = slice_at(s, 0, header_size)?.to_vec();
    copy.get_mut(16..20)?.fill(0);
    if crc32fast::hash(&copy) != stored_crc {
        return None;
    }
    let h = GptHeader {
        my_lba: u64_at(s, 24)?,
        first_usable: u64_at(s, 40)?,
        last_usable: u64_at(s, 48)?,
        entries_lba: u64_at(s, 72)?,
        entry_count: u32_at(s, 80)?,
        entry_size: u32_at(s, 84)?,
        entries_crc: u32_at(s, 88)?,
    };
    let array = u64::from(h.entry_count) * u64::from(h.entry_size);
    let sane = h.my_lba == expected_lba
        && h.entry_size >= 128
        && h.entry_size.is_multiple_of(8)
        && h.entry_count > 0
        && array <= MAX_GPT_ARRAY
        && h.entries_lba < total_sectors
        && h.first_usable <= h.last_usable
        && h.last_usable < total_sectors;
    sane.then_some(h)
}

/// Decodes the used entries of a CRC-checked GPT array.
pub fn parse_gpt_entries(array: &[u8], h: &GptHeader, ss: u64) -> Vec<Partition> {
    let size = usize::try_from(h.entry_size).unwrap_or(usize::MAX);
    let mut out = Vec::new();
    for (i, e) in array.chunks_exact(size).take(h.entry_count as usize).enumerate() {
        let (Some(type_guid), Some(unique_guid), Some(first), Some(last)) = (
            bytes_at::<16>(e, 0),
            bytes_at::<16>(e, 16),
            u64_at(e, 32),
            u64_at(e, 40),
        ) else {
            continue;
        };
        if type_guid == [0; 16] || last < first {
            continue;
        }
        let name: Vec<u16> = slice_at(e, 56, 72)
            .unwrap_or_default()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .take_while(|&c| c != 0)
            .collect();
        let name = String::from_utf16_lossy(&name);
        out.push(Partition {
            number: u32::try_from(i).unwrap_or(u32::MAX - 1) + 1,
            offset: first.saturating_mul(ss),
            size: (last - first + 1).saturating_mul(ss),
            kind: PartitionType::Gpt {
                type_guid,
                unique_guid,
            },
            name: (!name.is_empty()).then_some(name),
        });
    }
    out
}

/// Clips entries to the Source and reports overlaps.
fn finish(mut parts: Vec<Partition>, size: u64, warnings: &mut Vec<TableWarning>) -> Vec<Partition> {
    parts.retain(|p| {
        if p.offset >= size {
            warnings.push(TableWarning::EntryBeyondSource { number: p.number });
            return false;
        }
        true
    });
    for p in &mut parts {
        if p.offset.saturating_add(p.size) > size {
            warnings.push(TableWarning::EntryBeyondSource { number: p.number });
            p.size = size - p.offset;
        }
    }
    let mut sorted: Vec<&Partition> = parts.iter().collect();
    sorted.sort_by_key(|p| p.offset);
    for w in sorted.windows(2) {
        if let [a, b] = w
            && a.offset + a.size > b.offset
        {
            warnings.push(TableWarning::Overlap {
                a: a.number,
                b: b.number,
            });
        }
    }
    parts
}

#[cfg(test)]
mod tests;
