//! Importer for DMDE scan logs (`devscan.log`).
//!
//! This is interoperability with a data file, not with DMDE itself: nothing here is derived from
//! DMDE's code (clean-room rule in CONTRIBUTING.md). The format is INI-like with sections
//! `[devscan]`, `[fatscan]`, `[ntfsscan…]`, `[exfatscan]`, `[rawscan]`; long values continue on
//! the next line after a trailing backslash. Everything parsed here is an untrusted Hint
//! (ADR-0004): the importer only extracts candidates, the engines verify them on the Source.
#![forbid(unsafe_code)]

use rescyo_fs::{DirectorySeed, SeedOrigin};

/// Largest log accepted (the real ones are around 150 KB).
pub const MAX_LOG_BYTES: usize = 64 << 20;
const MAX_LINE: usize = 16 << 20;
const MAX_ITEMS: usize = 10_000_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DmdeError {
    #[error("the log is larger than {MAX_LOG_BYTES} bytes")]
    TooLarge,
    #[error("a value is longer than {MAX_LINE} bytes")]
    LineTooLong,
    #[error("not a DMDE scan log: no [devscan] section with scan_dev")]
    NotAScanLog,
    #[error("malformed {key}: {reason}")]
    Malformed { key: String, reason: String },
}

/// One `[section]` with its `key=value` pairs, in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub entries: Vec<(String, String)>,
}

impl Section {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// A FAT volume candidate from `[fatscan]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatVolume {
    pub index: u32,
    /// DMDE's file-system code (`FS:`); not trusted, the engine determines the type.
    pub fs_code: u32,
    pub cluster_size: u32,
    /// Absolute byte offset of cluster #2.
    pub data_offset: u64,
    /// Number of folders DMDE attributed to this candidate.
    pub folders: u64,
}

/// A directory start from `dirclus`: where it is, its cluster and the parent it claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirStart {
    pub offset: u64,
    pub cluster: u64,
    pub parent: u64,
}

/// What a scan log says that matters for recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmdeScan {
    pub sector_size: u32,
    pub sectors: u64,
    pub device: String,
    pub fat_volumes: Vec<FatVolume>,
    pub dir_starts: Vec<DirStart>,
    /// Directory fragments without `.` entries.
    pub dir_frags: Vec<u64>,
    /// Problems that did not stop the import (count mismatches and the like).
    pub warnings: Vec<String>,
}

impl DmdeScan {
    pub fn device_size(&self) -> u64 {
        self.sectors.saturating_mul(u64::from(self.sector_size))
    }

    /// The candidate with most folders, as DMDE's own best guess.
    pub fn best_volume(&self) -> Option<&FatVolume> {
        self.fat_volumes
            .iter()
            .max_by_key(|v| (v.folders, std::cmp::Reverse(v.index)))
    }

    /// Directory seeds that lie on `v`'s cluster grid; the others are dropped and counted.
    pub fn seeds_for(&self, v: &FatVolume) -> (Vec<DirectorySeed>, u64) {
        let c = u64::from(v.cluster_size);
        let mut seeds = Vec::new();
        let mut off_grid = 0u64;
        for d in &self.dir_starts {
            let on_grid = d.cluster >= 2
                && d.cluster
                    .checked_sub(2)
                    .and_then(|k| k.checked_mul(c))
                    .and_then(|x| x.checked_add(v.data_offset))
                    == Some(d.offset);
            if on_grid {
                seeds.push(DirectorySeed {
                    cluster: d.cluster,
                    parent: Some(d.parent),
                    origin: SeedOrigin::DmdeHint,
                });
            } else {
                off_grid += 1;
            }
        }
        for &o in &self.dir_frags {
            if let Some(rel) = o.checked_sub(v.data_offset)
                && rel.is_multiple_of(c)
            {
                seeds.push(DirectorySeed {
                    cluster: rel / c + 2,
                    parent: None,
                    origin: SeedOrigin::DmdeHint,
                });
            }
        }
        (seeds, off_grid)
    }
}

/// Splits a log into sections, joining continued lines. Accepts LF and CRLF.
pub fn parse_sections(bytes: &[u8]) -> Result<Vec<Section>, DmdeError> {
    if bytes.len() > MAX_LOG_BYTES {
        return Err(DmdeError::TooLarge);
    }
    let text = String::from_utf8_lossy(bytes);
    let mut sections: Vec<Section> = Vec::new();
    let mut pending = String::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(cont) = line.strip_suffix('\\') {
            if pending.len() + cont.len() > MAX_LINE {
                return Err(DmdeError::LineTooLong);
            }
            pending.push_str(cont);
            continue;
        }
        let full = if pending.is_empty() {
            line.to_owned()
        } else {
            std::mem::take(&mut pending) + line
        };
        if full.len() > MAX_LINE {
            return Err(DmdeError::LineTooLong);
        }
        let t = full.trim();
        if let Some(name) = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            sections.push(Section {
                name: name.to_owned(),
                entries: Vec::new(),
            });
        } else if let Some((k, v)) = t.split_once('=')
            && let Some(s) = sections.last_mut()
        {
            s.entries.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    Ok(sections)
}

fn malformed(key: &str, reason: &str) -> DmdeError {
    DmdeError::Malformed {
        key: key.to_owned(),
        reason: reason.to_owned(),
    }
}

fn num(s: &str) -> Option<u64> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(s)
}

/// Parses `N;item,item,…`, checking the count and the item limit.
fn counted_list<'a>(key: &str, v: &'a str, warnings: &mut Vec<String>) -> Result<Vec<&'a str>, DmdeError> {
    let v = unquote(v);
    let (n, rest) = v.split_once(';').ok_or_else(|| malformed(key, "missing count"))?;
    let n = num(n).ok_or_else(|| malformed(key, "bad count"))?;
    let items: Vec<&str> = rest
        .split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .take(MAX_ITEMS)
        .collect();
    if items.len() as u64 != n {
        warnings.push(format!("{key}: header says {n} items, found {}", items.len()));
    }
    Ok(items)
}

/// Parses a scan log into the parts the importer uses.
pub fn parse(bytes: &[u8]) -> Result<DmdeScan, DmdeError> {
    let sections = parse_sections(bytes)?;
    let dev = sections
        .iter()
        .find(|s| s.name == "devscan")
        .ok_or(DmdeError::NotAScanLog)?;
    let scan_dev = dev.get("scan_dev").ok_or(DmdeError::NotAScanLog)?;
    // a:b:c:d:e:<sector size>:<sectors>:"<device path>"
    let parts: Vec<&str> = scan_dev.splitn(8, ':').collect();
    let (Some(ss), Some(n)) = (
        parts.get(5).and_then(|x| num(x)),
        parts.get(6).and_then(|x| num(x)),
    ) else {
        return Err(malformed("scan_dev", "no sector size or count"));
    };
    let sector_size = u32::try_from(ss)
        .ok()
        .filter(|s| s.is_power_of_two() && (512..=65536).contains(s));
    let sector_size = sector_size.ok_or_else(|| malformed("scan_dev", "implausible sector size"))?;
    let device = parts.get(7).map(|p| unquote(p).to_owned()).unwrap_or_default();
    let mut out = DmdeScan {
        sector_size,
        sectors: n,
        device,
        fat_volumes: Vec::new(),
        dir_starts: Vec::new(),
        dir_frags: Vec::new(),
        warnings: Vec::new(),
    };
    let Some(fat) = sections.iter().find(|s| s.name == "fatscan") else {
        return Ok(out);
    };

    let count = fat.get("volumes_number").and_then(num).unwrap_or(0).min(4096);
    for i in 0..count {
        let key = format!("volume{i}");
        let Some(v) = fat.get(&key) else { continue };
        let mut fields = std::collections::HashMap::new();
        for f in unquote(v).split(';') {
            if let Some((k, x)) = f.split_once(':') {
                fields.insert(k.trim().to_ascii_uppercase(), x.trim().to_owned());
            }
        }
        let (Some(fs), Some(c), Some(start)) = (
            fields.get("FS").and_then(|x| num(x)),
            fields.get("CLUS").and_then(|x| num(x)),
            fields.get("START").and_then(|x| num(x)),
        ) else {
            out.warnings.push(format!("{key}: unreadable"));
            continue;
        };
        let Ok(cluster_size) = u32::try_from(c) else {
            continue;
        };
        if !cluster_size.is_power_of_two() || !(512..=(32 << 20)).contains(&cluster_size) {
            out.warnings.push(format!("{key}: implausible cluster size {c}"));
            continue;
        }
        let folders = fat
            .get(&format!("{key}_params"))
            .and_then(|p| unquote(p).split(';').nth(4).and_then(num))
            .unwrap_or(0);
        out.fat_volumes.push(FatVolume {
            index: u32::try_from(i).unwrap_or(u32::MAX),
            fs_code: u32::try_from(fs).unwrap_or(u32::MAX),
            cluster_size,
            data_offset: start,
            folders,
        });
    }
    if let Some(v) = fat.get("dirclus") {
        for item in counted_list("dirclus", v, &mut out.warnings)? {
            let mut p = item.split(':');
            match (
                p.next().and_then(num),
                p.next().and_then(num),
                p.next().and_then(num),
            ) {
                (Some(offset), Some(cluster), Some(parent)) => out.dir_starts.push(DirStart {
                    offset,
                    cluster,
                    parent,
                }),
                _ => out.warnings.push(format!("dirclus: unreadable item {item}")),
            }
        }
    }
    if let Some(v) = fat.get("dirfrags") {
        for item in counted_list("dirfrags", v, &mut out.warnings)? {
            match num(item) {
                Some(o) => out.dir_frags.push(o),
                None => out.warnings.push(format!("dirfrags: unreadable item {item}")),
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_lines_and_crlf() {
        let s = parse_sections(b"[a]\r\nk=\"1;x,\\\r\ny,\"\r\nother=2\n").unwrap();
        assert_eq!(s[0].get("k"), Some("\"1;x,y,\""));
        assert_eq!(s[0].get("other"), Some("2"));
    }

    #[test]
    fn rejects_non_logs_and_oversized_input() {
        assert_eq!(parse(b"[other]\nx=1\n"), Err(DmdeError::NotAScanLog));
        assert_eq!(parse(&vec![b'a'; MAX_LOG_BYTES + 1]), Err(DmdeError::TooLarge));
        assert!(matches!(
            parse(b"[devscan]\nscan_dev=1:2\n"),
            Err(DmdeError::Malformed { .. })
        ));
        assert!(matches!(
            parse(b"[devscan]\nscan_dev=1:0:11:-1:0x0:500:10:\"x\"\n"),
            Err(DmdeError::Malformed { .. })
        ));
    }

    #[test]
    fn counts_that_disagree_are_warnings_not_failures() {
        let log = b"[devscan]\nscan_dev=1:0:11:-1:0x0:512:100:\"d\"\n[fatscan]\ndirclus=\"5;0x1000:2:0,\"\n";
        let s = parse(log).unwrap();
        assert_eq!(s.dir_starts.len(), 1);
        assert_eq!(s.warnings.len(), 1);
    }
}
