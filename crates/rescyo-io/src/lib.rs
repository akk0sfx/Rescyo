//! Read-only block sources for Rescyo: disk images and physical disks.
//!
//! Nothing in this crate can write. [`BlockSource`] has no write operation, every file is
//! opened with read access only, and `clippy.toml` forbids write-capable `OpenOptions` here.
//! Engines never call a [`BlockSource`] directly: they read through a [`GuardedReader`], which
//! handles alignment, unreadable sectors, the [`BadSectorMap`] and the degradation alarm.

mod badmap;
mod fingerprint;
mod guarded;
mod image;
#[allow(unsafe_code)]
mod physical;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

use std::fmt;
use std::ops::Range;

pub use badmap::BadSectorMap;
pub use fingerprint::{FingerprintMatch, Sample, SourceFingerprint};
pub use guarded::{GuardError, GuardedReader, ReadPolicy, ReadReport};
pub use image::ImageSource;
pub use physical::{DiskInfo, PhysicalDisk, list_disks};

/// Size and sector layout of a Source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Total size in bytes.
    pub size: u64,
    /// Logical sector size in bytes: the unit of addressing and of unreadable areas.
    pub logical_sector: u32,
}

impl Geometry {
    /// Number of whole logical sectors.
    pub fn sectors(&self) -> u64 {
        self.size / u64::from(self.logical_sector)
    }
}

/// What kind of thing a Source is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A regular file holding a raw image.
    Image,
    /// A physical disk opened through the operating system.
    PhysicalDisk,
    /// An in-memory or fault-injecting source used in tests.
    Test,
}

/// Who a Source is: used for display, fingerprints and the write-isolation check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIdentity {
    pub kind: SourceKind,
    /// Path as given by the user (`/dev/sdb`, `\\.\PhysicalDrive2`, `flash.img`).
    pub path: String,
    pub model: Option<String>,
    pub serial: Option<String>,
}

/// Why a read failed.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The request reaches past the end of the Source.
    #[error("read beyond the end of the source")]
    OutOfBounds,
    /// The request is not aligned to the logical sector size.
    #[error("read not aligned to the logical sector size")]
    Misaligned,
    /// The medium could not deliver these sectors (CRC error, sector not found, I/O error).
    #[error("unreadable sectors")]
    Media,
    /// The device disappeared or stopped responding for good.
    #[error("source is gone: {0}")]
    Gone(std::io::Error),
    /// Any other operating-system error.
    #[error(transparent)]
    Io(std::io::Error),
}

/// A read-only Source of fixed-size logical sectors.
///
/// Implementations must be safe to call from several threads. There is deliberately no way to
/// write: see ADR-0003.
pub trait BlockSource: Send + Sync + fmt::Debug {
    fn geometry(&self) -> Geometry;
    fn identity(&self) -> &SourceIdentity;
    /// Reads exactly `buf.len()` bytes starting at `offset`.
    ///
    /// Both `offset` and `buf.len()` must be multiples of the logical sector size.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), ReadError>;
}

/// Byte range helper shared by the modules of this crate.
pub(crate) fn range_len(r: &Range<u64>) -> u64 {
    r.end.saturating_sub(r.start)
}

/// Whether a request starts and ends on sector boundaries.
pub(crate) fn is_aligned(offset: u64, len: usize, sector: u64) -> bool {
    offset.is_multiple_of(sector) && (len as u64).is_multiple_of(sector)
}
