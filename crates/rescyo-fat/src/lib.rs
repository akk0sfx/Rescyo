//! FAT12/16/32 and exFAT recovery engine.
//!
//! The behaviour follows the FlashRecover prototype: trust boot sectors when they agree, fall
//! back to backups and to allocation-table signatures, follow chains through the primary then
//! the mirror FAT, read a file as contiguous only when nothing else is left (and say so), parse
//! directories tolerantly, and hang folders whose parents are gone under `_LOST_FOLDERS`.
#![forbid(unsafe_code)]

mod boot;
pub mod dir;
pub mod exdir;
mod geometry;
mod infer;
mod scan;
mod table;
mod walk;

pub use boot::{Found, exfat_checksum_ok, parse_exfat_boot, parse_fat_boot, probe_boot};
pub use geometry::{FatCopy, FatGeometry, RootDir};
pub use infer::{geometry_from_grid, infer_volumes, seeds_for};
pub use scan::{DeepScan, DirStart, FatHead, deep_scan};
pub use table::{FatTable, Link, is_fat_head};
pub use walk::{WalkError, WalkOptions, WalkStats, walk};
