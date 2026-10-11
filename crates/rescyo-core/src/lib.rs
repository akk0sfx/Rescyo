//! Rescyo core: Sessions, Scans, Recovery and Reports, shared by every interface.
#![forbid(unsafe_code)]

pub mod recovery;
pub mod scan;
pub mod session;

pub use recovery::report::{Integrity, RecoveryReport};
pub use recovery::{
    DeletedMode, DeviceResolver, Plan, Problem, RecoverOptions, RecoveryError, RecoveryProgress, Selection,
    SystemResolver, check_destination, plan, run,
};
pub use scan::{Hints, Progress, ScanError, ScanMode, ScanOptions, ScanReport, VolumeSummary, scan};
pub use session::{Session, SessionError, SourceRecord, StoredEntry, VolumeRow};
