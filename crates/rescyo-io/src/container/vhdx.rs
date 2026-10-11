//! VHDX ([MS-VHDX] v1). Fields are little-endian; structures carry CRC-32C checksums.
//!
//! Two copies of the header and of the region table: the valid header with the higher
//! sequence number wins, a damaged region table is replaced by its copy. A metadata log that
//! was not replayed (the image of a VM that crashed) is replayed in memory only, so the disk
//! reads as the hypervisor would have left it after recovery; nothing is written to the file.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use super::{
    ContainerFormat, HostFile, Layout, OpenError, Opened, ParentRef, Piece, dir_of, foreign_path_candidates,
};
use crate::ReadError;
use crate::bytes::{bytes_at, slice_at, u16_at, u32_at, u64_at};

const F: ContainerFormat = ContainerFormat::Vhdx;
const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;
const PAGE: u64 = 4 * KIB;

const GUID_BAT: [u8; 16] = guid(
    0x2DC2_7766,
    0xF623,
    0x4200,
    [0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08],
);
const GUID_METADATA: [u8; 16] = guid(
    0x8B7C_A206,
    0x4790,
    0x4B9A,
    [0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88, 0x6E],
);
const GUID_FILE_PARAMS: [u8; 16] = guid(
    0xCAA1_6737,
    0xFA36,
    0x4D43,
    [0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7, 0x6B],
);
const GUID_DISK_SIZE: [u8; 16] = guid(
    0x2FA5_4224,
    0xCD1B,
    0x4876,
    [0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4, 0xB8],
);
const GUID_LOGICAL_SECTOR: [u8; 16] = guid(
    0x8141_BF1D,
    0xA96F,
    0x4709,
    [0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB, 0x5F],
);
const GUID_PARENT_LOCATOR: [u8; 16] = guid(
    0xA8D3_5F2D,
    0xB30B,
    0x454D,
    [0xAB, 0xF7, 0xD3, 0xD8, 0x48, 0x34, 0xAB, 0x0C],
);

/// A GUID in its on-disk (mixed-endian) byte order.
const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6],
        d4[7],
    ]
}

/// CRC-32C (Castagnoli), as VHDX checksums use.
pub(crate) fn crc32c(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = u32::try_from(i).unwrap_or(0);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0x82F6_3B78 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    });
    let mut crc = !0u32;
    for &b in data {
        let i = usize::from(crc.to_le_bytes()[0] ^ b);
        crc = table.get(i).copied().unwrap_or(0) ^ (crc >> 8);
    }
    !crc
}

/// Checksum of a structure whose own checksum field (bytes 4..8) counts as zero.
fn checksum_ok(buf: &[u8]) -> bool {
    let Some(stored) = u32_at(buf, 4) else {
        return false;
    };
    let mut copy = buf.to_vec();
    if let Some(field) = copy.get_mut(4..8) {
        field.fill(0);
    }
    crc32c(&copy) == stored
}

#[derive(Debug, Clone, Copy)]
struct Header {
    sequence: u64,
    log_guid: [u8; 16],
    log_length: u64,
    log_offset: u64,
}

fn parse_header(b: &[u8]) -> Option<Header> {
    if slice_at(b, 0, 4)? != b"head" || !checksum_ok(b) {
        return None;
    }
    if u16_at(b, 66)? != 1 {
        return None;
    }
    Some(Header {
        sequence: u64_at(b, 8)?,
        log_guid: bytes_at(b, 48)?,
        log_length: u64::from(u32_at(b, 68)?),
        log_offset: u64_at(b, 72)?,
    })
}

/// File-offset pages rewritten by the log replay; `None` is a page of zeros.
type Overlay = BTreeMap<u64, Option<Vec<u8>>>;

/// Reads `len` bytes at `offset` with the replayed log applied.
fn read_overlaid(file: &HostFile, overlay: &Overlay, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = file.read_vec(offset, len)?;
    let end = offset + len as u64;
    let first = offset - offset % PAGE;
    for (&page, data) in overlay.range(first..end) {
        for i in 0..PAGE {
            let at = page + i;
            if at < offset || at >= end {
                continue;
            }
            let v = data
                .as_ref()
                .and_then(|d| d.get(usize::try_from(i).ok()?).copied())
                .unwrap_or(0);
            if let Some(slot) = usize::try_from(at - offset).ok().and_then(|k| buf.get_mut(k)) {
                *slot = v;
            }
        }
    }
    Ok(buf)
}

pub(super) fn open(path: &Path) -> Result<Opened, OpenError> {
    let host = HostFile::open(path)?;
    let mut notes = Vec::new();
    let ident = host.read_vec(0, 8)?;
    if ident != b"vhdxfile" {
        return Err(OpenError::invalid(F, "file type identifier is missing"));
    }
    let h1 = host
        .read_vec(64 * KIB, 4096)
        .ok()
        .as_deref()
        .and_then(parse_header);
    let h2 = host
        .read_vec(128 * KIB, 4096)
        .ok()
        .as_deref()
        .and_then(parse_header);
    let header = match (h1, h2) {
        (Some(a), Some(b)) => {
            if a.sequence >= b.sequence {
                a
            } else {
                b
            }
        }
        (Some(a), None) | (None, Some(a)) => {
            notes.push("one of the two headers is damaged; used the other".into());
            a
        }
        (None, None) => return Err(OpenError::invalid(F, "both headers are damaged")),
    };

    let overlay = if header.log_guid == [0; 16] || header.log_length == 0 {
        Overlay::new()
    } else {
        match replay_log(&host, &header) {
            Some((o, entries)) if entries > 0 => {
                notes.push(format!(
                    "the image was not closed cleanly: {entries} log entries replayed in memory (the file is unchanged)"
                ));
                o
            }
            Some(_) => Overlay::new(),
            None => {
                notes.push("the metadata log could not be replayed; recent changes may be missing".into());
                Overlay::new()
            }
        }
    };

    let regions = [192 * KIB, 256 * KIB]
        .into_iter()
        .filter_map(|at| host.read_vec(at, usize::try_from(64 * KIB).ok()?).ok())
        .find(|r| slice_at(r, 0, 4) == Some(b"regi".as_slice()) && checksum_ok(r))
        .ok_or_else(|| OpenError::invalid(F, "both region tables are damaged"))?;
    let count = u32_at(&regions, 8).unwrap_or(0).min(2047);
    let (mut bat_region, mut meta_region) = (None, None);
    for i in 0..usize::try_from(count).unwrap_or(0) {
        let e = 16 + i * 32;
        let (Some(id), Some(off), Some(len), Some(req)) = (
            bytes_at::<16>(&regions, e),
            u64_at(&regions, e + 16),
            u32_at(&regions, e + 24),
            u32_at(&regions, e + 28),
        ) else {
            break;
        };
        if id == GUID_BAT {
            bat_region = Some((off, u64::from(len)));
        } else if id == GUID_METADATA {
            meta_region = Some((off, u64::from(len)));
        } else if req & 1 != 0 {
            return Err(OpenError::unsupported(F, "an unknown required region"));
        }
    }
    let (meta_at, meta_len) = meta_region.ok_or_else(|| OpenError::invalid(F, "no metadata region"))?;
    let (bat_at, bat_len) = bat_region.ok_or_else(|| OpenError::invalid(F, "no BAT region"))?;
    let meta = read_overlaid(
        &host,
        &overlay,
        meta_at,
        usize::try_from(meta_len.min(MIB)).unwrap_or(0),
    )
    .map_err(|_| OpenError::invalid(F, "metadata region is outside the file"))?;
    if slice_at(&meta, 0, 8) != Some(b"metadata".as_slice()) {
        return Err(OpenError::invalid(F, "metadata table signature is wrong"));
    }
    let item = |want: [u8; 16]| -> Option<&[u8]> {
        let n = usize::from(u16_at(&meta, 10)?).min(2047);
        (0..n).find_map(|i| {
            let e = 32 + i * 32;
            if bytes_at::<16>(&meta, e)? != want {
                return None;
            }
            let off = usize::try_from(u32_at(&meta, e + 16)?).ok()?;
            let len = usize::try_from(u32_at(&meta, e + 20)?).ok()?;
            slice_at(&meta, off, len)
        })
    };
    let missing = |what: &str| OpenError::invalid(F, format!("metadata item {what} is missing"));
    let params = item(GUID_FILE_PARAMS).ok_or_else(|| missing("File Parameters"))?;
    let block_size = u64::from(u32_at(params, 0).ok_or_else(|| missing("block size"))?);
    let has_parent = u32_at(params, 4).is_some_and(|f| f & 2 != 0);
    let size = item(GUID_DISK_SIZE)
        .and_then(|b| u64_at(b, 0))
        .ok_or_else(|| missing("Virtual Disk Size"))?;
    let sector = item(GUID_LOGICAL_SECTOR)
        .and_then(|b| u32_at(b, 0))
        .ok_or_else(|| missing("Logical Sector Size"))?;
    if !(MIB..=256 * MIB).contains(&block_size) || !block_size.is_power_of_two() {
        return Err(OpenError::invalid(F, format!("block size {block_size}")));
    }
    if sector != 512 && sector != 4096 {
        return Err(OpenError::invalid(F, format!("logical sector size {sector}")));
    }
    let chunk_ratio = ((1u64 << 23) * u64::from(sector)) / block_size;
    let blocks = size.div_ceil(block_size);
    let entries = if has_parent {
        blocks.div_ceil(chunk_ratio).saturating_mul(chunk_ratio + 1)
    } else {
        blocks + blocks.saturating_sub(1) / chunk_ratio
    };
    let entries = entries.min(bat_len / 8).min(host.len / 8);
    let raw = read_overlaid(&host, &overlay, bat_at, usize::try_from(entries * 8).unwrap_or(0))
        .map_err(|_| OpenError::invalid(F, "BAT is outside the file"))?;
    let bat: Vec<u64> = raw
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_le_bytes(*c))
        .collect();

    let parent = if has_parent {
        let locator = item(GUID_PARENT_LOCATOR).ok_or_else(|| missing("Parent Locator"))?;
        Some(ParentRef {
            candidates: parent_candidates(path, locator),
            check: None,
        })
    } else {
        None
    };
    Ok(Opened {
        files: vec![host],
        layout: Box::new(VhdxLayout {
            size,
            block_size,
            sector: u64::from(sector),
            chunk_ratio,
            bat,
            differencing: has_parent,
            overlay,
            bitmaps: Mutex::new(Vec::new()),
        }),
        logical_sector: sector,
        parent,
        notes,
    })
}

/// Parent locator key/value pairs (UTF-16LE): `relative_path`, `absolute_win32_path`,
/// `volume_path`, tried in that order.
fn parent_candidates(path: &Path, loc: &[u8]) -> Vec<std::path::PathBuf> {
    let utf16 = |off: u32, len: u16| -> Option<String> {
        let b = slice_at(loc, usize::try_from(off).ok()?, usize::from(len))?;
        let units: Vec<u16> = b
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        Some(String::from_utf16_lossy(&units))
    };
    let n = usize::from(u16_at(loc, 18).unwrap_or(0)).min(256);
    let mut pairs = Vec::new();
    for i in 0..n {
        let e = 20 + i * 12;
        let (Some(ko), Some(vo), Some(kl), Some(vl)) = (
            u32_at(loc, e),
            u32_at(loc, e + 4),
            u16_at(loc, e + 8),
            u16_at(loc, e + 10),
        ) else {
            break;
        };
        if let (Some(k), Some(v)) = (utf16(ko, kl), utf16(vo, vl)) {
            pairs.push((k, v));
        }
    }
    let dir = dir_of(path);
    let mut out = Vec::new();
    for key in ["relative_path", "absolute_win32_path", "volume_path"] {
        for (_, v) in pairs.iter().filter(|(k, _)| k == key) {
            for c in foreign_path_candidates(&dir, v) {
                if !out.contains(&c) {
                    out.push(c);
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
struct LogEntry {
    offset: u64,
    length: u64,
    tail: u64,
    sequence: u64,
}

/// Replays the active log sequence into an overlay. `None` when the log is unreadable.
fn replay_log(host: &HostFile, h: &Header) -> Option<(Overlay, usize)> {
    if !h.log_length.is_multiple_of(MIB) || h.log_length > 1 << 32 {
        return None;
    }
    let log = host
        .read_vec(h.log_offset, usize::try_from(h.log_length).ok()?)
        .ok()?;
    let len = h.log_length;
    let at = |off: u64, n: u64| -> Option<Vec<u8>> {
        // Entries may wrap around the end of the circular log.
        (0..n)
            .map(|i| log.get(usize::try_from((off + i) % len).ok()?).copied())
            .collect()
    };
    let parse = |off: u64| -> Option<LogEntry> {
        let head = at(off, 64)?;
        if slice_at(&head, 0, 4)? != b"loge" || bytes_at::<16>(&head, 32)? != h.log_guid {
            return None;
        }
        let length = u64::from(u32_at(&head, 8)?);
        if length == 0 || !length.is_multiple_of(PAGE) || length > len {
            return None;
        }
        if !checksum_ok(&at(off, length)?) {
            return None;
        }
        Some(LogEntry {
            offset: off,
            length,
            tail: u64::from(u32_at(&head, 12)?),
            sequence: u64_at(&head, 16)?,
        })
    };
    let mut valid: Vec<LogEntry> = (0..len / PAGE).filter_map(|p| parse(p * PAGE)).collect();
    valid.sort_by_key(|e| std::cmp::Reverse(e.sequence));
    // The active sequence: the newest entry whose chain from its tail is complete.
    for head in &valid {
        let mut chain = Vec::new();
        let mut off = head.tail;
        let mut guard = 0;
        while guard < 1_000_000 {
            guard += 1;
            let Some(e) = parse(off) else { break };
            if chain
                .last()
                .is_some_and(|p: &LogEntry| e.sequence != p.sequence + 1)
            {
                break;
            }
            let done = e.sequence == head.sequence;
            off = (e.offset + e.length) % len;
            chain.push(e);
            if done {
                let mut overlay = Overlay::new();
                for e in &chain {
                    apply(&at(e.offset, e.length)?, e.sequence, &mut overlay)?;
                }
                return Some((overlay, chain.len()));
            }
        }
    }
    Some((Overlay::new(), 0))
}

/// Applies one entry's zero and data descriptors to the overlay.
fn apply(entry: &[u8], sequence: u64, overlay: &mut Overlay) -> Option<()> {
    let count = u64::from(u32_at(entry, 24)?);
    let desc_area = (64 + 32 * count).div_ceil(PAGE) * PAGE;
    let mut data_at = desc_area;
    for i in 0..count {
        let d = usize::try_from(64 + i * 32).ok()?;
        match slice_at(entry, d, 4)? {
            b"zero" => {
                let zero_len = u64_at(entry, d + 8)?;
                let file_off = u64_at(entry, d + 16)?;
                for p in 0..zero_len / PAGE {
                    overlay.insert(file_off + p * PAGE, None);
                }
            }
            b"desc" => {
                let trailing = bytes_at::<4>(entry, d + 4)?;
                let leading = bytes_at::<8>(entry, d + 8)?;
                let file_off = u64_at(entry, d + 16)?;
                let sector = slice_at(entry, usize::try_from(data_at).ok()?, 4096)?;
                if slice_at(sector, 0, 4)? != b"data" {
                    return None;
                }
                let seq = (u64::from(u32_at(sector, 4)?) << 32) | u64::from(u32_at(sector, 4092)?);
                if seq != sequence {
                    return None;
                }
                let mut page = Vec::with_capacity(4096);
                page.extend_from_slice(&leading);
                page.extend_from_slice(slice_at(sector, 8, 4084)?);
                page.extend_from_slice(&trailing);
                overlay.insert(file_off, Some(page));
                data_at += PAGE;
            }
            _ => return None,
        }
    }
    Some(())
}

#[derive(Debug)]
struct VhdxLayout {
    size: u64,
    block_size: u64,
    sector: u64,
    chunk_ratio: u64,
    bat: Vec<u64>,
    differencing: bool,
    overlay: Overlay,
    bitmaps: Mutex<Vec<(u64, Vec<u8>)>>,
}

const NOT_PRESENT: u64 = 0;
const FULLY_PRESENT: u64 = 6;
const PARTIALLY_PRESENT: u64 = 7;
const ZERO: u64 = 2;

impl VhdxLayout {
    fn entry(&self, index: u64) -> u64 {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.bat.get(i).copied())
            .unwrap_or(0)
    }

    /// Host bytes from `at`, honouring pages rewritten by the log.
    fn host_piece(&self, at: u64, run: u64) -> (Piece, u64) {
        let page = at - at % PAGE;
        if self.overlay.contains_key(&page) {
            return (
                Piece::Chunk {
                    chunk: page,
                    within: at - page,
                },
                run.min(page + PAGE - at),
            );
        }
        let limit = self
            .overlay
            .range(at..at + run)
            .next()
            .map_or(run, |(&p, _)| p - at);
        (Piece::Data { file: 0, offset: at }, limit)
    }

    fn bitmap(&self, files: &[HostFile], chunk: u64, at: u64) -> Result<Vec<u8>, ReadError> {
        if let Ok(c) = self.bitmaps.lock()
            && let Some((_, b)) = c.iter().find(|(k, _)| *k == chunk)
        {
            return Ok(b.clone());
        }
        let file = files.first().ok_or(ReadError::Media)?;
        let b = read_overlaid(
            file,
            &self.overlay,
            at,
            usize::try_from(MIB).map_err(|_| ReadError::Media)?,
        )
        .map_err(|_| ReadError::Media)?;
        if let Ok(mut c) = self.bitmaps.lock() {
            if c.len() >= 4 {
                c.remove(0);
            }
            c.push((chunk, b.clone()));
        }
        Ok(b)
    }
}

impl Layout for VhdxLayout {
    fn size(&self) -> u64 {
        self.size
    }

    fn locate(&self, files: &[HostFile], offset: u64) -> Result<(Piece, u64), ReadError> {
        let block = offset / self.block_size;
        let within = offset % self.block_size;
        let run = (self.block_size - within).min(self.size - offset);
        let entry = self.entry(block + block / self.chunk_ratio);
        let state = entry & 7;
        let host = (entry >> 20) << 20;
        let absent = if self.differencing {
            Piece::Parent
        } else {
            Piece::Zero
        };
        match state {
            FULLY_PRESENT if host != 0 => Ok(self.host_piece(host + within, run)),
            PARTIALLY_PRESENT if self.differencing && host != 0 => {
                let chunk = block / self.chunk_ratio;
                let bm_entry = self.entry((chunk + 1).saturating_mul(self.chunk_ratio + 1) - 1);
                if bm_entry & 7 != FULLY_PRESENT {
                    return Ok((Piece::Bad, run));
                }
                let bitmap = self.bitmap(files, chunk, (bm_entry >> 20) << 20)?;
                // One bit per logical sector of the chunk.
                let first = ((block % self.chunk_ratio) * self.block_size + within) / self.sector;
                let count = run / self.sector;
                let bit = |s: u64| -> bool {
                    usize::try_from(s / 8)
                        .ok()
                        .and_then(|i| bitmap.get(i))
                        .is_some_and(|b| b & (1 << (s % 8)) != 0)
                };
                let here = bit(first);
                let same = (first..first + count).take_while(|&s| bit(s) == here).count() as u64;
                let len = (same * self.sector).max(self.sector).min(run);
                if here {
                    Ok(self.host_piece(host + within, len))
                } else {
                    Ok((Piece::Parent, len))
                }
            }
            NOT_PRESENT => Ok((absent, run)),
            ZERO => Ok((Piece::Zero, run)),
            // UNDEFINED and UNMAPPED: nothing reliable is stored here.
            1 | 3 => Ok((absent, run)),
            _ => Ok((Piece::Bad, run)),
        }
    }

    fn decode(&self, _: &[HostFile], page: u64) -> Result<Vec<u8>, ReadError> {
        match self.overlay.get(&page) {
            Some(Some(d)) => Ok(d.clone()),
            Some(None) => Ok(vec![0; 4096]),
            None => Err(ReadError::Media),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::testutil::{file_in, read_all};
    use crate::container::{ContainerFormat, open_image};
    use crate::testing::pattern;

    const MIB_U32: u32 = 1 << 20;

    fn put(img: &mut Vec<u8>, at: u64, bytes: &[u8]) {
        let at = usize::try_from(at).unwrap();
        if img.len() < at + bytes.len() {
            img.resize(at + bytes.len(), 0);
        }
        img[at..at + bytes.len()].copy_from_slice(bytes);
    }

    fn seal(buf: &mut [u8]) {
        buf[4..8].fill(0);
        let c = crc32c(buf);
        buf[4..8].copy_from_slice(&c.to_le_bytes());
    }

    fn header(seq: u64, log_guid: [u8; 16]) -> Vec<u8> {
        let mut h = vec![0u8; 4096];
        h[0..4].copy_from_slice(b"head");
        h[8..16].copy_from_slice(&seq.to_le_bytes());
        h[48..64].copy_from_slice(&log_guid);
        h[66..68].copy_from_slice(&1u16.to_le_bytes());
        h[68..72].copy_from_slice(&MIB_U32.to_le_bytes());
        h[72..80].copy_from_slice(&MIB.to_le_bytes());
        seal(&mut h);
        h
    }

    /// A 3 MiB dynamic disk with 1 MiB blocks: log at 1 MiB, metadata at 2 MiB, BAT at 3 MiB,
    /// data blocks from 4 MiB. `blocks[i]` is the first 4 KiB of block i (the rest zero).
    fn build(blocks: &[Option<Vec<u8>>], log_guid: [u8; 16]) -> Vec<u8> {
        let mut img = Vec::new();
        put(&mut img, 0, b"vhdxfile");
        put(&mut img, 64 * KIB, &header(1, log_guid));
        put(&mut img, 128 * KIB, &header(2, log_guid));
        let mut r = vec![0u8; 64 * 1024];
        r[0..4].copy_from_slice(b"regi");
        r[8..12].copy_from_slice(&2u32.to_le_bytes());
        r[16..32].copy_from_slice(&GUID_BAT);
        r[32..40].copy_from_slice(&(3 * MIB).to_le_bytes());
        r[40..44].copy_from_slice(&MIB_U32.to_le_bytes());
        r[44..48].copy_from_slice(&1u32.to_le_bytes());
        r[48..64].copy_from_slice(&GUID_METADATA);
        r[64..72].copy_from_slice(&(2 * MIB).to_le_bytes());
        r[72..76].copy_from_slice(&MIB_U32.to_le_bytes());
        r[76..80].copy_from_slice(&1u32.to_le_bytes());
        seal(&mut r);
        put(&mut img, 192 * KIB, &r);
        put(&mut img, 256 * KIB, &r);
        let mut m = vec![0u8; 64 * 1024];
        m[0..8].copy_from_slice(b"metadata");
        m[10..12].copy_from_slice(&3u16.to_le_bytes());
        let items: [([u8; 16], Vec<u8>); 3] = [
            (
                GUID_FILE_PARAMS,
                [MIB_U32.to_le_bytes(), 0u32.to_le_bytes()].concat(),
            ),
            (GUID_DISK_SIZE, (blocks.len() as u64 * MIB).to_le_bytes().to_vec()),
            (GUID_LOGICAL_SECTOR, 512u32.to_le_bytes().to_vec()),
        ];
        for (i, (id, val)) in items.iter().enumerate() {
            let e = 32 + i * 32;
            let off = 0x10000 + i * 64;
            m[e..e + 16].copy_from_slice(id);
            m[e + 16..e + 20].copy_from_slice(&u32::try_from(off).unwrap().to_le_bytes());
            m[e + 20..e + 24].copy_from_slice(&u32::try_from(val.len()).unwrap().to_le_bytes());
            put(&mut m, off as u64, val);
        }
        put(&mut img, 2 * MIB, &m);
        let mut bat = vec![0u8; blocks.len() * 8];
        for (i, b) in blocks.iter().enumerate() {
            if let Some(b) = b {
                let at = (4 + i as u64) * MIB;
                put(&mut img, at, b);
                let e = (at >> 20 << 20) | FULLY_PRESENT;
                bat[i * 8..i * 8 + 8].copy_from_slice(&e.to_le_bytes());
            }
        }
        put(&mut img, 3 * MIB, &bat);
        img.resize(img.len().next_multiple_of(usize::try_from(MIB).unwrap()), 0);
        img
    }

    #[test]
    fn crc32c_matches_the_reference_value() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    #[test]
    fn dynamic_disk_reads_present_blocks_and_zeros() {
        let dir = tempfile::tempdir().unwrap();
        let img = build(&[Some(pattern(4096)), None, Some(vec![4; 4096])], [0; 16]);
        let p = file_in(dir.path(), "d.vhdx", &img);
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert_eq!(info.format, ContainerFormat::Vhdx);
        assert!(info.notes.is_empty(), "{:?}", info.notes);
        let all = read_all(src.as_ref());
        assert_eq!(&all[..4096], &pattern(4096)[..]);
        assert!(all[4096..2 * 1024 * 1024].iter().all(|&b| b == 0));
        assert_eq!(&all[2 * 1024 * 1024..2 * 1024 * 1024 + 4096], &[4u8; 4096][..]);
    }

    #[test]
    fn damaged_newer_header_falls_back_to_the_older() {
        let dir = tempfile::tempdir().unwrap();
        let mut img = build(&[Some(pattern(4096))], [0; 16]);
        img[128 * 1024 + 100] ^= 1;
        let p = file_in(dir.path(), "h.vhdx", &img);
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert!(
            info.notes.iter().any(|n| n.contains("header")),
            "{:?}",
            info.notes
        );
        assert_eq!(&read_all(src.as_ref())[..4096], &pattern(4096)[..]);
    }

    #[test]
    fn unreplayed_log_is_applied_in_memory_only() {
        let dir = tempfile::tempdir().unwrap();
        let lg = [0x42u8; 16];
        let mut img = build(&[Some(pattern(4096))], lg);
        // One log entry: a data descriptor rewriting the first page of block 0 (at 4 MiB).
        let mut e = vec![0u8; 8192];
        e[0..4].copy_from_slice(b"loge");
        e[8..12].copy_from_slice(&8192u32.to_le_bytes());
        e[12..16].copy_from_slice(&0u32.to_le_bytes());
        e[16..24].copy_from_slice(&5u64.to_le_bytes());
        e[24..28].copy_from_slice(&1u32.to_le_bytes());
        e[32..48].copy_from_slice(&lg);
        let new_page = vec![0xEEu8; 4096];
        e[64..68].copy_from_slice(b"desc");
        e[68..72].copy_from_slice(
            &new_page[4092..]
                .try_into()
                .map(u32::from_le_bytes)
                .unwrap()
                .to_le_bytes(),
        );
        e[72..80].copy_from_slice(&new_page[..8]);
        e[80..88].copy_from_slice(&(4 * MIB).to_le_bytes());
        e[88..96].copy_from_slice(&5u64.to_le_bytes());
        e[4096..4100].copy_from_slice(b"data");
        e[4100..4104].copy_from_slice(&0u32.to_le_bytes());
        e[4104..8188].copy_from_slice(&new_page[8..4092]);
        e[8188..8192].copy_from_slice(&5u32.to_le_bytes());
        seal(&mut e);
        put(&mut img, MIB, &e);
        let before = img.clone();
        let p = file_in(dir.path(), "l.vhdx", &img);
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert!(
            info.notes.iter().any(|n| n.contains("replayed")),
            "{:?}",
            info.notes
        );
        assert_eq!(&read_all(src.as_ref())[..4096], &new_page[..]);
        assert_eq!(std::fs::read(&p).unwrap(), before, "the file must not change");
    }

    #[test]
    fn both_region_tables_damaged_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut img = build(&[None], [0; 16]);
        img[192 * 1024 + 40] ^= 1;
        img[256 * 1024 + 40] ^= 1;
        let p = file_in(dir.path(), "r.vhdx", &img);
        assert!(matches!(
            open_image(&p, 512, None),
            Err(OpenError::Invalid { .. })
        ));
    }
}
