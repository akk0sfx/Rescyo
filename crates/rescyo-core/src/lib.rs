//! Rescyo core: Sessions, Scans, Recovery and Reports, shared by every interface.
#![forbid(unsafe_code)]

pub mod browse;
pub mod imaging;
pub mod import;
pub mod partsearch;
pub mod recovery;
pub mod scan;
pub mod session;

pub use imaging::{ImageOptions, ImagePhase, ImageProgress, ImageReport, ImagingError, image};
pub use import::{ImportError, ImportOptions, ImportSummary, import_dmde};
pub use partsearch::{BootScanner, FoundPartition, PartitionStatus};
pub use recovery::report::{Integrity, RecoveryReport};
pub use recovery::{
    DeletedMode, DeviceResolver, Plan, Problem, RecoverOptions, RecoveryError, RecoveryProgress, Selection,
    SystemResolver, check_destination, plan, run,
};
pub use scan::{
    Hints, PartitionReport, Progress, ScanError, ScanMode, ScanOptions, ScanReport, VolumeSummary,
    find_partitions, scan,
};
pub use session::{Session, SessionError, SourceRecord, StoredEntry, VolumeRow};
