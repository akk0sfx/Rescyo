//! Import of DMDE scan logs: their geometry and folder list become Hints for a Scan.

use rescyo_dmde::DmdeScan;
use rescyo_fs::CancelToken;
use rescyo_io::GuardedReader;
use serde::Serialize;

use crate::scan::{Hints, Progress, ScanError, ScanMode, ScanOptions, ScanReport, scan};
use crate::session::Session;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error(
        "the log describes {log_sectors} sectors of {log_sector_size} bytes, the source has {sectors} of {sector_size}"
    )]
    GeometryMismatch {
        log_sectors: u64,
        log_sector_size: u32,
        sectors: u64,
        sector_size: u32,
    },
    #[error("the log has no FAT volume candidate {0}")]
    NoSuchVolume(u32),
    #[error("the log has no FAT volume candidates")]
    NoVolumes,
    #[error(transparent)]
    Scan(#[from] ScanError),
}

/// What the import took from the log, for the journal and the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportSummary {
    pub volume_index: u32,
    pub cluster_size: u32,
    pub data_offset: u64,
    pub dmde_folders: u64,
    pub seeds: u64,
    pub off_grid: u64,
    pub warnings: Vec<String>,
}

/// How to import a log.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Which `[fatscan]` volume candidate to use; DMDE's best (most folders) by default.
    pub volume: Option<u32>,
    /// Accept a log whose device geometry differs from the Source (recorded in the journal).
    pub force: bool,
    pub scan: ScanOptions,
}

/// Checks the log against the Source, then scans with its chosen volume as a hint.
pub fn import_dmde(
    r: &GuardedReader,
    session: &mut Session,
    log: &DmdeScan,
    io: &ImportOptions,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<(ImportSummary, ScanReport), ImportError> {
    let (volume, force, base) = (io.volume, io.force, &io.scan);
    let g = r.geometry();
    if !force && (log.sector_size != g.logical_sector || log.device_size() != g.size) {
        return Err(ImportError::GeometryMismatch {
            log_sectors: log.sectors,
            log_sector_size: log.sector_size,
            sectors: g.sectors(),
            sector_size: g.logical_sector,
        });
    }
    let v = match volume {
        Some(i) => log
            .fat_volumes
            .iter()
            .find(|v| v.index == i)
            .ok_or(ImportError::NoSuchVolume(i))?,
        None => log.best_volume().ok_or(ImportError::NoVolumes)?,
    };
    let (seeds, off_grid) = log.seeds_for(v);
    let summary = ImportSummary {
        volume_index: v.index,
        cluster_size: v.cluster_size,
        data_offset: v.data_offset,
        dmde_folders: v.folders,
        seeds: seeds.len() as u64,
        off_grid,
        warnings: log.warnings.clone(),
    };
    session
        .journal("dmde_import", &serde_json::to_value(&summary).unwrap_or_default())
        .map_err(ScanError::from)?;
    if force && (log.sector_size != g.logical_sector || log.device_size() != g.size) {
        session
            .journal(
                "dmde_geometry_forced",
                &serde_json::json!({ "log_bytes": log.device_size(), "source_bytes": g.size }),
            )
            .map_err(ScanError::from)?;
    }
    let opts = ScanOptions {
        mode: if base.mode == ScanMode::Deep {
            ScanMode::Deep
        } else {
            ScanMode::Quick
        },
        hints: Hints {
            fat_grids: vec![(v.cluster_size, v.data_offset)],
            seeds,
            source: "dmde".into(),
        },
        ..base.clone()
    };
    let report = scan(r, session, &opts, progress, cancel)?;
    Ok((summary, report))
}
