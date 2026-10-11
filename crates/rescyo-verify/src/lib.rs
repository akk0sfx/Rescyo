//! Content integrity checks for recovered files.
//!
//! A recovered file can be the right size and still be garbage: a wrong cluster, a guessed
//! layout, a sector of zeros. These checks read the file the way an application would and say
//! whether it holds up. Formats with checksums (PNG, ZIP-based documents) are verified exactly;
//! JPEG is decoded strictly; GIF, PDF and MP4 are checked structurally.
//!
//! Every input is hostile: sizes are bounded, decompression is capped, nothing recurses deeply.
#![forbid(unsafe_code)]

mod gif;
mod jpeg;
mod mp4;
mod pdf;
mod png;
mod zip;

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// What a check concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The file reads correctly as its format.
    Intact,
    /// The file is not valid as its format; the reason is for the Report.
    Damaged(String),
    /// No check exists for this format, or the file is too large to check.
    Unverified,
}

impl Verdict {
    fn damaged(why: impl Into<String>) -> Self {
        Self::Damaged(why.into())
    }
}

/// Formats Rescyo can verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Gif,
    Zip,
    Pdf,
    Mp4,
}

impl Format {
    /// The format a file name promises.
    pub fn from_name(name: &str) -> Option<Self> {
        let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
        Some(match ext.as_str() {
            "jpg" | "jpeg" | "jpe" | "jfif" => Self::Jpeg,
            "png" => Self::Png,
            "gif" => Self::Gif,
            "zip" | "docx" | "xlsx" | "pptx" | "docm" | "xlsm" | "pptm" | "odt" | "ods" | "odp" | "odg"
            | "epub" | "jar" | "apk" | "xpi" | "kmz" | "3mf" => Self::Zip,
            "pdf" => Self::Pdf,
            "mp4" | "m4v" | "m4a" | "mov" | "3gp" | "3g2" | "heic" | "heif" | "avif" => Self::Mp4,
            _ => return None,
        })
    }

    /// The format the first bytes indicate.
    pub fn sniff(head: &[u8]) -> Option<Self> {
        if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Self::Jpeg)
        } else if head.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if head.starts_with(b"PK\x03\x04") {
            Some(Self::Zip)
        } else if head.starts_with(b"%PDF-") {
            Some(Self::Pdf)
        } else if head
            .get(4..8)
            .is_some_and(|t| matches!(t, b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip"))
        {
            Some(Self::Mp4)
        } else {
            None
        }
    }
}

/// Random access to the bytes being checked.
pub trait Input: Read + Seek {}
impl<T: Read + Seek> Input for T {}

/// Reads exactly `buf.len()` bytes at `off`.
pub(crate) fn read_at(r: &mut dyn Input, off: u64, buf: &mut [u8]) -> io::Result<()> {
    r.seek(SeekFrom::Start(off))?;
    r.read_exact(buf)
}

/// Checks `len` bytes of `r`, expecting the format the name promises (or, without a known
/// extension, the format the content shows).
pub fn verify(r: &mut dyn Input, len: u64, name: &str) -> Verdict {
    let mut head = [0u8; 16];
    let n = usize::try_from(len.min(16)).unwrap_or(0);
    if read_at(r, 0, head.get_mut(..n).unwrap_or_default()).is_err() {
        return Verdict::damaged("unreadable");
    }
    let sniffed = Format::sniff(head.get(..n).unwrap_or_default());
    let format = match (Format::from_name(name), sniffed) {
        (Some(expected), Some(found)) if expected != found => {
            return Verdict::damaged(format!("content is {found:?}, not {expected:?}"));
        }
        (Some(expected), None) if len > 0 => {
            return Verdict::damaged(format!("does not start like {expected:?}"));
        }
        (Some(f), _) | (None, Some(f)) => f,
        (None, None) => return Verdict::Unverified,
    };
    let result = match format {
        Format::Jpeg => jpeg::check(r, len),
        Format::Png => png::check(r, len),
        Format::Gif => gif::check(r, len),
        Format::Zip => zip::check(r, len),
        Format::Pdf => pdf::check(r, len),
        Format::Mp4 => mp4::check(r, len),
    };
    result.unwrap_or_else(|e| Verdict::damaged(format!("read error: {e}")))
}

/// Checks a file on disk (typically one Rescyo just recovered).
pub fn verify_file(path: &Path, name: &str) -> Verdict {
    match File::open(path).and_then(|f| Ok((f.metadata()?.len(), f))) {
        Ok((len, f)) => verify(&mut io::BufReader::new(f), len, name),
        Err(e) => Verdict::damaged(format!("cannot open: {e}")),
    }
}

#[cfg(test)]
mod tests;
