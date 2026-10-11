//! Scan: find volumes on a Source and fill a Session with their entries.

use std::ops::Range;

use rescyo_fat::{FatGeometry, FatScanner};
use rescyo_fs::codepage::Codepage;
use rescyo_fs::{CancelToken, DirectorySeed, Evidence, FsKind, Limits, Phase, ScanSink, VolumeCandidate};
use rescyo_io::{GuardError, GuardedReader};
use rescyo_ntfs::{NtfsGeometry, NtfsScanner};
use rescyo_part::TableKind;
use serde::Serialize;

use crate::partsearch::{BootScanner, FoundPartition, PartitionStatus};
use crate::session::{Session, SessionError};

const CHUNK: u64 = 4 << 20;
/// How far around a hinted data area allocation tables are searched for.
const HINT_HEAD: u64 = 512 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanMode {
    /// Partition starts and their boot sectors only.
    Quick,
    /// Also read the whole Source once for lost volumes, tables and directories.
    Deep,
}

/// Geometry and directory starts supplied from outside (a DMDE log), all to be verified.
#[derive(Debug, Clone, Default)]
pub struct Hints {
    /// (cluster size, absolute offset of FAT cluster #2) pairs.
    pub fat_grids: Vec<(u32, u64)>,
    pub seeds: Vec<DirectorySeed>,
    /// Name of the hint source for evidence, e.g. `dmde`.
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub mode: ScanMode,
    pub codepage: Option<Codepage>,
    pub locale_codepage: Option<Codepage>,
    /// Candidates below this score are recorded but not walked.
    pub min_score: u64,
    pub limits: Limits,
    /// During a deep scan, also carve files by their signatures (into a separate `carved`
    /// pseudo-volume, skipping files the file systems already describe).
    pub carve: bool,
    pub hints: Hints,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            mode: ScanMode::Quick,
            codepage: None,
            locale_codepage: None,
            min_score: 250,
            limits: Limits::default(),
            carve: false,
            hints: Hints::default(),
        }
    }
}

/// Progress events for interfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VolumeSummary {
    pub id: i64,
    pub fs: FsKind,
    pub offset: Option<u64>,
    pub cluster_size: u32,
    pub data_offset: u64,
    pub score: u64,
    pub walked: bool,
    pub files: u64,
    pub dirs: u64,
    pub deleted: u64,
    pub lost_folders: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanReport {
    pub partition_table: &'static str,
    pub mode: ScanMode,
    pub volumes: Vec<VolumeSummary>,
    pub unreadable_sectors: u64,
    /// Volumes found by their boot sectors during a deep scan, with their table status.
    pub partitions: Vec<FoundPartition>,
}

/// What a partition search found.
#[derive(Debug, Clone, Serialize)]
pub struct PartitionReport {
    pub partition_table: &'static str,
    /// Volumes found by their boot sectors.
    pub found: Vec<FoundPartition>,
    /// Table entries with no volume found at their start: (number, offset, size).
    pub unmatched: Vec<(u32, u64, u64)>,
    pub unreadable_sectors: u64,
}

/// Reads the whole Source once for boot sectors and compares what they describe with the
/// partition table. Nothing is written.
pub fn find_partitions(
    r: &GuardedReader,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<PartitionReport, ScanError> {
    let size = r.geometry().size;
    let table = rescyo_part::read_partitions(r)?;
    let mut boots = BootScanner::new();
    read_through(r, 0..size, cancel, progress, |at, chunk| boots.feed(at, chunk))?;
    let found = boots.finish(r, &table)?;
    let unmatched = table
        .partitions
        .iter()
        .filter(|p| !found.iter().any(|f| f.offset == p.offset))
        .map(|p| (p.number, p.offset, p.size))
        .collect();
    Ok(PartitionReport {
        partition_table: table_name(table.kind),
        found,
        unmatched,
        unreadable_sectors: r.bad_map().bad_sectors(),
    })
}

fn table_name(k: TableKind) -> &'static str {
    match k {
        TableKind::Mbr => "mbr",
        TableKind::Gpt => "gpt",
        TableKind::None => "none",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("the source degraded: {errors} unreadable sectors; image it first or continue explicitly")]
    Degraded { errors: u64 },
    #[error("the source disappeared: {0}")]
    Gone(std::io::Error),
    #[error("read error: {0}")]
    Io(std::io::Error),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Engine(String),
}

impl From<GuardError> for ScanError {
    fn from(e: GuardError) -> Self {
        match e {
            GuardError::DegradationAlarm { errors } => Self::Degraded { errors },
            GuardError::Gone(e) => Self::Gone(e),
            GuardError::Io(e) => Self::Io(e),
            GuardError::OutOfBounds => Self::Engine("read out of bounds".into()),
        }
    }
}

/// A candidate with the engine geometry that produced it.
#[derive(Debug, Clone)]
enum Cand {
    Fat(FatGeometry, Vec<Evidence>, u64),
    Ntfs(NtfsGeometry, Vec<Evidence>, u64),
    Ext4(rescyo_ext4::Ext4Geometry, Vec<Evidence>, u64),
}

impl Cand {
    fn score(&self) -> u64 {
        match self {
            Self::Fat(_, _, s) | Self::Ntfs(_, _, s) | Self::Ext4(_, _, s) => *s,
        }
    }

    fn candidate(&self) -> VolumeCandidate {
        match self {
            Self::Fat(g, e, s) => g.to_candidate(e.clone(), *s),
            Self::Ntfs(g, e, s) => g.to_candidate(e.clone(), *s),
            Self::Ext4(g, e, s) => g.to_candidate(e.clone(), *s),
        }
    }

    /// The byte range the volume's data occupies, for overlap checks.
    fn extent(&self) -> Range<u64> {
        match self {
            Self::Fat(g, _, _) => g.volume_offset.unwrap_or(g.data_offset)..g.data_end(),
            Self::Ntfs(g, _, _) => g.volume_offset..g.volume_offset + g.size,
            Self::Ext4(g, _, _) => g.volume_offset..g.volume_offset + g.size(),
        }
    }

    fn same(&self, o: &Self) -> bool {
        match (self, o) {
            (Self::Fat(a, _, _), Self::Fat(b, _, _)) => {
                a.data_offset == b.data_offset && a.cluster_size == b.cluster_size
            }
            (Self::Ntfs(a, _, _), Self::Ntfs(b, _, _)) => a.volume_offset == b.volume_offset,
            (Self::Ext4(a, _, _), Self::Ext4(b, _, _)) => a.volume_offset == b.volume_offset,
            _ => false,
        }
    }
}

struct Forward<'a> {
    f: &'a mut dyn FnMut(Progress),
}

impl ScanSink for Forward<'_> {
    fn entry(&mut self, _: rescyo_fs::Entry) -> Result<rescyo_fs::EntryId, rescyo_fs::SinkError> {
        Err(rescyo_fs::SinkError("not an entry sink".into()))
    }

    fn progress(&mut self, phase: Phase, done: u64, total: Option<u64>) {
        (self.f)(Progress { phase, done, total });
    }
}

/// Finds volumes and walks the best non-overlapping ones into `session`.
pub fn scan(
    r: &GuardedReader,
    session: &mut Session,
    opts: &ScanOptions,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<ScanReport, ScanError> {
    let size = r.geometry().size;
    let table = rescyo_part::read_partitions(r)?;
    session.journal(
        "partition_table",
        &serde_json::json!({ "kind": format!("{:?}", table.kind), "partitions": table.partitions.len() }),
    )?;
    let mut regions: Vec<(u64, u64)> = table
        .partitions
        .iter()
        .map(|p| (p.offset, p.offset + p.size))
        .collect();
    regions.push((0, size));

    progress(Progress {
        phase: Phase::Probe,
        done: 0,
        total: Some(regions.len() as u64),
    });
    let mut cands: Vec<Cand> = Vec::new();
    for &(start, end) in &regions {
        for f in rescyo_fat::probe_boot(r, start)? {
            cands.push(Cand::Fat(f.geometry, f.evidence, f.score));
        }
        for f in rescyo_ntfs::probe_boot(r, start, Some(end))? {
            cands.push(Cand::Ntfs(f.geometry, f.evidence, f.score));
        }
        for f in rescyo_ext4::probe(r, start)? {
            cands.push(Cand::Ext4(f.geometry, f.evidence, f.score));
        }
    }

    // One sequential pass serves every deep scanner.
    let (mut fat_scan, mut ntfs_scan) = (None, None);
    let mut partitions = Vec::new();
    let mut carver = None;
    if opts.mode == ScanMode::Deep {
        let mut fs = FatScanner::new(0..size);
        let mut ns = NtfsScanner::new(0..size);
        let mut boots = BootScanner::new();
        let mut carving = opts.carve.then(rescyo_carve::Carver::new);
        read_through(r, 0..size, cancel, progress, |at, chunk| {
            fs.feed(at, chunk);
            ns.feed(at, chunk);
            boots.feed(at, chunk);
            if let Some(c) = carving.as_mut() {
                c.feed(at, chunk);
            }
        })?;
        carver = carving;
        // Volumes found by their boot sectors (lost partitions included) are probed like
        // table entries, so they carry boot-sector evidence.
        partitions = boots.finish(r, &table)?;
        for p in partitions.iter().filter(|p| p.status != PartitionStatus::Nested) {
            for f in rescyo_fat::probe_boot(r, p.offset)? {
                cands.push(Cand::Fat(f.geometry, f.evidence, f.score));
            }
            for f in rescyo_ntfs::probe_boot(r, p.offset, Some(p.offset + p.size))? {
                cands.push(Cand::Ntfs(f.geometry, f.evidence, f.score));
            }
            for f in rescyo_ext4::probe(r, p.offset)? {
                cands.push(Cand::Ext4(f.geometry, f.evidence, f.score));
            }
        }
        session.journal(
            "partitions_found",
            &serde_json::to_value(&partitions).unwrap_or_default(),
        )?;
        let fs = fs.finish();
        let ns = ns.finish();
        for f in rescyo_fat::infer_volumes(r, &fs)? {
            cands.push(Cand::Fat(f.geometry, f.evidence, f.score));
        }
        for f in rescyo_ntfs::infer_volumes(r, &ns) {
            cands.push(Cand::Ntfs(f.geometry, f.evidence, f.score));
        }
        fat_scan = Some(fs);
        ntfs_scan = Some(ns);
    }

    // Hinted grids: confirm allocation tables in front of each hinted data area.
    for &(cluster, data) in &opts.hints.fat_grids {
        let head = match &fat_scan {
            Some(s) => s.clone(),
            None => {
                let lo = data.saturating_sub(HINT_HEAD);
                let mut fs = FatScanner::new(lo..data.min(size));
                let mut fwd = |_p: Progress| {};
                read_through(r, lo..data.min(size), cancel, &mut fwd, |at, chunk| {
                    fs.feed(at, chunk)
                })?;
                fs.finish()
            }
        };
        let end = regions
            .iter()
            .filter(|(s, e)| *s <= data && data < *e)
            .map(|r| r.1)
            .min()
            .unwrap_or(size);
        if let Some(mut f) = rescyo_fat::geometry_from_grid(r, &head, cluster, data, end, None)? {
            f.evidence.push(Evidence::Hint {
                source: opts.hints.source.clone(),
            });
            f.score += 400;
            cands.push(Cand::Fat(f.geometry, f.evidence, f.score));
        }
    }

    // Merge duplicates (keeping the best score and all evidence), then pick volumes.
    let mut merged: Vec<Cand> = Vec::new();
    cands.sort_by_key(|c| std::cmp::Reverse(c.score()));
    for c in cands {
        if let Some(m) = merged.iter_mut().find(|m| m.same(&c)) {
            let extra: Vec<Evidence> = match &c {
                Cand::Fat(_, e, _) | Cand::Ntfs(_, e, _) | Cand::Ext4(_, e, _) => e.clone(),
            };
            let (Cand::Fat(_, ev, _) | Cand::Ntfs(_, ev, _) | Cand::Ext4(_, ev, _)) = m;
            for e in extra {
                if !ev.contains(&e) {
                    ev.push(e);
                }
            }
        } else {
            merged.push(c);
        }
    }

    let mut report = ScanReport {
        partition_table: table_name(table.kind),
        mode: opts.mode,
        volumes: Vec::new(),
        unreadable_sectors: 0,
        partitions,
    };
    let mut chosen: Vec<Range<u64>> = Vec::new();
    for c in &merged {
        let ext = c.extent();
        let overlaps = chosen.iter().any(|x| x.start < ext.end && ext.start < x.end);
        let walk = c.score() >= opts.min_score && !overlaps;
        let id = session.add_volume(&c.candidate())?;
        let mut summary = VolumeSummary {
            id,
            fs: c.candidate().fs,
            offset: c.candidate().offset,
            cluster_size: c.candidate().cluster_size,
            data_offset: c.candidate().data_offset,
            score: c.score(),
            walked: false,
            files: 0,
            dirs: 0,
            deleted: 0,
            lost_folders: 0,
        };
        if walk {
            chosen.push(ext);
            walk_one(
                r,
                session,
                id,
                c,
                opts,
                fat_scan.as_ref(),
                ntfs_scan.as_ref(),
                progress,
                cancel,
                &mut summary,
            )?;
            session.mark_walked(id)?;
            summary.walked = true;
        }
        report.volumes.push(summary);
    }
    if let Some(c) = carver {
        report.volumes.extend(carve(r, session, c, cancel)?);
    }
    let bad = r.bad_map();
    report.unreadable_sectors = bad.bad_sectors();
    session.save_bad_map(&bad)?;
    session.journal(
        "scan_finished",
        &serde_json::to_value(&report).unwrap_or_default(),
    )?;
    Ok(report)
}

/// Carves files outside what the walked volumes describe into a `carved` pseudo-volume.
fn carve(
    r: &GuardedReader,
    session: &mut Session,
    carver: rescyo_carve::Carver,
    cancel: &CancelToken,
) -> Result<Option<VolumeSummary>, ScanError> {
    let known: std::collections::HashSet<u64> = session
        .entries(|e| e.entry.kind == rescyo_fs::EntryKind::File)?
        .iter()
        .filter_map(|e| match e.entry.layout.extents.first() {
            Some(rescyo_fs::Extent::Disk { offset, .. }) => Some(*offset),
            _ => None,
        })
        .collect();
    let files = carver.finish(r, &known, cancel).map_err(|e| match e {
        rescyo_carve::CarveError::Read(g) => ScanError::from(g),
        rescyo_carve::CarveError::Cancelled => ScanError::Cancelled,
        other => ScanError::Engine(other.to_string()),
    })?;
    if files.is_empty() {
        return Ok(None);
    }
    let candidate = VolumeCandidate {
        fs: FsKind::Carved,
        offset: None,
        cluster_size: 512,
        data_offset: 0,
        size: None,
        label: None,
        evidence: Vec::new(),
        score: 0,
        params: serde_json::Value::Null,
    };
    let id = session.add_volume(&candidate)?;
    let mut sink = session.sink(id)?;
    let stats = rescyo_carve::emit(&files, &mut sink).map_err(|e| ScanError::Engine(e.to_string()))?;
    sink.finish()?;
    session.mark_walked(id)?;
    session.journal("carved", &serde_json::to_value(&stats).unwrap_or_default())?;
    Ok(Some(VolumeSummary {
        id,
        fs: FsKind::Carved,
        offset: None,
        cluster_size: 512,
        data_offset: 0,
        score: 0,
        walked: true,
        files: files.len() as u64,
        dirs: stats.len() as u64,
        deleted: files.len() as u64,
        lost_folders: 0,
    }))
}

#[allow(
    clippy::too_many_arguments,
    reason = "internal step of `scan` sharing its state"
)]
fn walk_one(
    r: &GuardedReader,
    session: &mut Session,
    id: i64,
    c: &Cand,
    opts: &ScanOptions,
    fat_scan: Option<&rescyo_fat::DeepScan>,
    ntfs_scan: Option<&rescyo_ntfs::MftScan>,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
    summary: &mut VolumeSummary,
) -> Result<(), ScanError> {
    let mut sink = session.sink(id)?;
    sink.on_progress(|phase, done, total| progress(Progress { phase, done, total }));
    match c {
        Cand::Fat(g, _, _) => {
            let mut seeds = fat_scan.map(|s| rescyo_fat::seeds_for(g, s)).unwrap_or_default();
            seeds.extend(opts.hints.seeds.iter().copied());
            let wo = rescyo_fat::WalkOptions {
                codepage: opts.codepage,
                locale_codepage: opts.locale_codepage,
                seeds,
            };
            let st = rescyo_fat::walk(r, g, &wo, &opts.limits, &mut sink, cancel).map_err(|e| match e {
                rescyo_fat::WalkError::Read(g) => ScanError::from(g),
                rescyo_fat::WalkError::Cancelled => ScanError::Cancelled,
                other => ScanError::Engine(other.to_string()),
            })?;
            (summary.files, summary.dirs, summary.deleted, summary.lost_folders) =
                (st.files, st.dirs, st.deleted, st.lost_folders);
        }
        Cand::Ntfs(g, _, _) => {
            let st =
                rescyo_ntfs::walk(r, g, ntfs_scan, &opts.limits, &mut sink, cancel).map_err(|e| match e {
                    rescyo_ntfs::WalkError::Read(g) => ScanError::from(g),
                    rescyo_ntfs::WalkError::Cancelled => ScanError::Cancelled,
                    other => ScanError::Engine(other.to_string()),
                })?;
            (summary.files, summary.dirs, summary.deleted, summary.lost_folders) =
                (st.files, st.dirs, st.deleted, st.lost_folders);
        }
        Cand::Ext4(g, _, _) => {
            let st = rescyo_ext4::walk(r, g, &opts.limits, &mut sink, cancel).map_err(|e| match e {
                rescyo_ext4::WalkError::Read(g) => ScanError::from(g),
                rescyo_ext4::WalkError::Cancelled => ScanError::Cancelled,
                other => ScanError::Engine(other.to_string()),
            })?;
            (summary.files, summary.dirs, summary.deleted) = (st.files, st.dirs, st.deleted);
        }
    }
    sink.finish()?;
    Ok(())
}

/// Reads `range` sequentially in large chunks, handing each to `feed`.
fn read_through(
    r: &GuardedReader,
    range: Range<u64>,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(Progress),
    mut feed: impl FnMut(u64, &[u8]),
) -> Result<(), ScanError> {
    let start = range.start - range.start % 512;
    let total = range.end.saturating_sub(start);
    let mut buf = vec![0u8; usize::try_from(CHUNK).unwrap_or(1 << 20)];
    let mut at = start;
    let mut fwd = Forward { f: progress };
    while at < range.end {
        if cancel.is_cancelled() {
            return Err(ScanError::Cancelled);
        }
        let n = usize::try_from((range.end - at).min(CHUNK)).unwrap_or(0);
        let chunk = buf.get_mut(..n).unwrap_or_default();
        r.read(at, chunk)?;
        feed(at, chunk);
        at += n as u64;
        fwd.progress(Phase::DeepScan, at - start, Some(total));
    }
    Ok(())
}
