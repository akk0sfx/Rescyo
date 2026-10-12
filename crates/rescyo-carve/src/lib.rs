//! File carving: files found by their own structure where no file system describes them.
//!
//! A sequential pass looks for file signatures at sector starts (where file systems place
//! files). Each hit is then followed through its format (JPEG segments, PNG chunks, ZIP records,
//! MP4 atoms…) to find where it ends; a hit whose structure breaks before a proper end is
//! dropped rather than guessed. Carved files are assumed contiguous, so recovery verifies their
//! content and reports the result.
#![forbid(unsafe_code)]

mod cursor;
mod end;

use std::collections::{BTreeMap, HashSet};

use rescyo_fs::{
    AssemblyMethod, CancelToken, DataLayout, Entry, EntryKind, EntryState, Parent, ScanSink, SinkError, Times,
};
use rescyo_io::{GuardError, GuardedReader};

use crate::cursor::Cursor;

const SECTOR: usize = 512;
/// Signature hits kept from one pass.
const MAX_HITS: usize = 2_000_000;

/// Formats Rescyo can carve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Format {
    Jpeg,
    Png,
    Gif,
    Pdf,
    Zip,
    Mp4,
    Riff,
    Bmp,
}

impl Format {
    /// The format whose signature starts `s`, if any.
    pub fn sniff(s: &[u8]) -> Option<Self> {
        let at = |o: usize, sig: &[u8]| s.get(o..o + sig.len()) == Some(sig);
        if at(0, &[0xFF, 0xD8, 0xFF]) && s.get(3).is_some_and(|&m| m >= 0xC0 && m != 0xFF) {
            Some(Self::Jpeg)
        } else if at(0, b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if at(0, b"GIF87a") || at(0, b"GIF89a") {
            Some(Self::Gif)
        } else if at(0, b"%PDF-1.") || at(0, b"%PDF-2.") {
            Some(Self::Pdf)
        } else if at(0, b"PK\x03\x04") {
            Some(Self::Zip)
        } else if at(4, b"ftyp")
            && s.get(8..12)
                .is_some_and(|b| b.iter().all(|c| c.is_ascii_graphic() || *c == b' '))
        {
            Some(Self::Mp4)
        } else if at(0, b"RIFF") && (at(8, b"WAVE") || at(8, b"AVI ") || at(8, b"WEBP")) {
            Some(Self::Riff)
        } else if at(0, b"BM") && at(6, &[0, 0, 0, 0]) {
            Some(Self::Bmp)
        } else {
            None
        }
    }

    /// The largest file of this format followed (a cap against hostile structures).
    pub fn max_size(self) -> u64 {
        match self {
            Self::Jpeg | Self::Gif | Self::Bmp => 256 << 20,
            Self::Png => 512 << 20,
            Self::Pdf => 1 << 30,
            Self::Zip | Self::Riff => 4 << 30,
            Self::Mp4 => 64 << 30,
        }
    }
}

/// A carved file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carved {
    pub offset: u64,
    pub len: u64,
    pub format: Format,
    /// Extension from the content (`docx` for a Word package, `mov` for a QuickTime movie…).
    pub ext: &'static str,
}

/// Collects signature hits from a sequential pass.
#[derive(Debug, Default)]
pub struct Carver {
    hits: Vec<(u64, Format)>,
}

impl Carver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks at the start of every 512-byte sector of `chunk`, which starts at absolute `at`.
    pub fn feed(&mut self, at: u64, chunk: &[u8]) {
        let skip = usize::try_from((SECTOR as u64 - at % SECTOR as u64) % SECTOR as u64).unwrap_or(0);
        let Some(aligned) = chunk.get(skip..) else { return };
        for (i, s) in aligned.chunks(SECTOR).enumerate() {
            if self.hits.len() >= MAX_HITS {
                return;
            }
            if let Some(f) = Format::sniff(s) {
                self.hits.push((at + (skip + i * SECTOR) as u64, f));
            }
        }
    }

    pub fn hits(&self) -> usize {
        self.hits.len()
    }

    /// Follows every hit not already `known` (files the file systems describe) to its end.
    pub fn finish(
        self,
        r: &GuardedReader,
        known: &HashSet<u64>,
        cancel: &CancelToken,
    ) -> Result<Vec<Carved>, CarveError> {
        let mut out = Vec::new();
        let mut covered_until = 0u64;
        for (offset, format) in self.hits {
            if cancel.is_cancelled() {
                return Err(CarveError::Cancelled);
            }
            // Inside a file already carved (a thumbnail in a JPEG, a member of a ZIP): skip.
            if offset < covered_until || known.contains(&offset) {
                continue;
            }
            let mut c = Cursor::new(r, offset, format.max_size());
            let len = end::length(&mut c, format);
            if let Some(e) = c.failed.take() {
                return Err(CarveError::Read(e));
            }
            // A size field may claim more than the Source holds: such a file is truncated.
            let Some(len) = len.filter(|&l| offset.checked_add(l).is_some_and(|e| e <= r.geometry().size))
            else {
                continue;
            };
            let ext = extension(&mut c, format, len);
            if let Some(e) = c.failed.take() {
                return Err(CarveError::Read(e));
            }
            let Some(ext) = ext else { continue };
            covered_until = offset + len;
            out.push(Carved {
                offset,
                len,
                format,
                ext,
            });
        }
        Ok(out)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CarveError {
    #[error(transparent)]
    Read(#[from] GuardError),
    #[error(transparent)]
    Sink(#[from] SinkError),
    #[error("cancelled")]
    Cancelled,
}

/// The extension the content calls for, or `None` for a container Rescyo does not recognise.
fn extension(c: &mut Cursor<'_>, format: Format, len: u64) -> Option<&'static str> {
    Some(match format {
        Format::Jpeg => "jpg",
        Format::Png => "png",
        Format::Gif => "gif",
        Format::Pdf => "pdf",
        Format::Bmp => "bmp",
        Format::Riff => match c.bytes(8, 4)?.as_slice() {
            b"WAVE" => "wav",
            b"AVI " => "avi",
            _ => "webp",
        },
        Format::Mp4 => match c.bytes(8, 4)?.as_slice() {
            b"qt  " => "mov",
            b"heic" | b"heix" | b"mif1" | b"msf1" => "heic",
            b"avif" => "avif",
            b"M4A " => "m4a",
            b if b.starts_with(b"3g") => "3gp",
            _ => "mp4",
        },
        Format::Zip => {
            // Package kind from the member names near the start.
            let head = c.bytes(0, usize::try_from(len.min(64 << 10)).unwrap_or(0))?;
            let has = |n: &[u8]| head.windows(n.len()).any(|w| w == n);
            if has(b"[Content_Types].xml") {
                if has(b"word/") {
                    "docx"
                } else if has(b"xl/") {
                    "xlsx"
                } else if has(b"ppt/") {
                    "pptx"
                } else {
                    "zip"
                }
            } else if has(b"application/vnd.oasis.opendocument.text") {
                "odt"
            } else if has(b"application/vnd.oasis.opendocument.spreadsheet") {
                "ods"
            } else if has(b"application/vnd.oasis.opendocument.presentation") {
                "odp"
            } else if has(b"application/epub+zip") {
                "epub"
            } else if has(b"AndroidManifest.xml") {
                "apk"
            } else if has(b"META-INF/MANIFEST.MF") {
                "jar"
            } else {
                "zip"
            }
        }
    })
}

/// How many files of each extension were carved.
pub type CarveStats = BTreeMap<&'static str, u64>;

/// Emits carved files into `sink`: one folder per extension, files named by their offset.
pub fn emit(files: &[Carved], sink: &mut dyn ScanSink) -> Result<CarveStats, CarveError> {
    let mut dirs: BTreeMap<&'static str, rescyo_fs::EntryId> = BTreeMap::new();
    let mut stats = CarveStats::new();
    for f in files {
        let dir = match dirs.get(f.ext) {
            Some(d) => *d,
            None => {
                let id = sink.entry(Entry {
                    parent: Parent::Root,
                    name: f.ext.to_owned(),
                    name_raw: None,
                    kind: EntryKind::Directory,
                    state: EntryState::Deleted,
                    size: 0,
                    times: Times::default(),
                    layout: DataLayout::default(),
                    assembly: AssemblyMethod::Carved,
                    fs_ref: 0,
                    seed: None,
                })?;
                dirs.insert(f.ext, id);
                id
            }
        };
        let mut layout = DataLayout::default();
        layout.push_disk(f.offset, f.len);
        sink.entry(Entry {
            parent: Parent::Entry(dir),
            name: format!("{:012x}.{}", f.offset, f.ext),
            name_raw: None,
            kind: EntryKind::File,
            state: EntryState::Deleted,
            size: f.len,
            times: Times::default(),
            layout,
            assembly: AssemblyMethod::Carved,
            fs_ref: f.offset,
            seed: None,
        })?;
        *stats.entry(f.ext).or_default() += 1;
    }
    Ok(stats)
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, reason = "test fixtures")]
mod tests;
