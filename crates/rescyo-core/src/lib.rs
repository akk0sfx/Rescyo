//! Rescyo core: Sessions, Scans, Recovery and Reports, shared by every interface.
#![forbid(unsafe_code)]

pub mod scan;
pub mod session;

pub use scan::{Hints, Progress, ScanError, ScanMode, ScanOptions, ScanReport, VolumeSummary, scan};
pub use session::{Session, SessionError, SourceRecord, StoredEntry, VolumeRow};
