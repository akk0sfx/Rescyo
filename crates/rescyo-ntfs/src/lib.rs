//! NTFS recovery engine: the MFT, parent references and data runlists.
//!
//! v0.1 reads resident and non-resident unnamed data streams, sparse runs, extension records,
//! and deleted records. Compressed and encrypted streams, alternate data streams and the
//! journals arrive in v0.2.
#![forbid(unsafe_code)]

mod boot;
pub mod record;
mod scan;
mod walk;

pub use boot::{Found, NtfsGeometry, parse_boot, probe_boot};
pub use scan::{MftScan, RecordHit, deep_scan, infer_volumes};
pub use walk::{WalkError, WalkStats, walk};
