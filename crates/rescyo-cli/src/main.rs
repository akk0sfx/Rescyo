//! Rescyo command-line interface.
//!
//! Exit codes: 0 success; 1 internal error; 2 usage error; 3 source problem (cannot open, does
//! not match the session, degraded, gone); 4 preflight failed (nothing written); 5 partial
//! success (some files partial, damaged or failed); 130 interrupted (the session is kept).
#![forbid(unsafe_code)]
#![allow(
    clippy::result_large_err,
    reason = "a Failure is built once, on the way out of the process"
)]

mod dto;
mod out;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use rescyo_core::{
    DeletedMode, ImportError, ImportOptions, RecoverOptions, ScanMode, ScanOptions, Selection, Session,
    SessionError, SystemResolver,
};
use rescyo_fs::codepage::Codepage;
use rescyo_fs::{CancelToken, EntryKind, EntryState};
use rescyo_i18n::{Arg, size};
use rescyo_io::{
    BlockSource, FingerprintMatch, GuardedReader, ImageSource, PhysicalDisk, ReadPolicy, SourceFingerprint,
};

use out::{Failure, Out};

#[derive(Debug, Parser)]
#[command(
    name = "rescyo",
    version,
    about = "Free, open-source data recovery that never writes to your disk."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    #[command(flatten)]
    global: Global,
}

#[derive(Debug, Args)]
struct Global {
    /// Print one JSON envelope (schema version 1) instead of text.
    #[arg(long, global = true)]
    json: bool,
    /// Print progress as JSON lines, then the final envelope.
    #[arg(long, global = true)]
    json_stream: bool,
    /// Interface language: en, ru, zh-CN, ja, ko (default: the system's).
    #[arg(long, global = true)]
    lang: Option<String>,
    /// Do not print progress.
    #[arg(long, short, global = true)]
    quiet: bool,
}

#[derive(Debug, Args)]
struct SourceArgs {
    /// Logical sector size of an image file (512 or 4096).
    #[arg(long, default_value_t = 512)]
    sector_size: u32,
    /// Keep reading when many sectors are unreadable (disables the degradation alarm).
    #[arg(long)]
    continue_on_errors: bool,
    /// Extra read attempts for each unreadable sector (default 0: read each sector once).
    #[arg(long, default_value_t = 0)]
    retries: u32,
}

#[derive(Debug, Args)]
struct SelectArgs {
    /// Include deleted entries.
    #[arg(long, conflicts_with = "only_deleted")]
    deleted: bool,
    /// Only deleted entries.
    #[arg(long)]
    only_deleted: bool,
    /// Only these extensions, comma-separated (jpg,docx).
    #[arg(long, value_delimiter = ',')]
    ext: Vec<String>,
    /// Only paths under this folder (as shown by `ls`).
    #[arg(long)]
    path: Option<String>,
    /// Only this volume (from `volumes`).
    #[arg(long)]
    volume: Option<i64>,
}

impl SelectArgs {
    fn selection(&self) -> Selection {
        Selection {
            deleted: if self.only_deleted {
                DeletedMode::Only
            } else if self.deleted {
                DeletedMode::Include
            } else {
                DeletedMode::Exclude
            },
            extensions: self
                .ext
                .iter()
                .map(|e| e.trim().trim_start_matches('.').to_lowercase())
                .collect(),
            path_prefix: self.path.clone(),
            volume: self.volume,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List disks (reading them may need administrator or root rights).
    Disks,
    /// Find volumes on a disk or image and save what is found to a new session file.
    Scan {
        /// Disk (/dev/sdb, /dev/rdisk3, \\.\PhysicalDrive2) or image file.
        source: String,
        /// New session file to create (never overwritten).
        #[arg(long)]
        session: PathBuf,
        /// Also read the whole source for lost volumes and folders.
        #[arg(long)]
        deep: bool,
        /// Code page of short names (cp437, cp866, cp932, cp936, cp949, cp950, cp850).
        #[arg(long)]
        codepage: Option<String>,
        /// Allow a session file on a disk Rescyo cannot identify.
        #[arg(long)]
        allow_unverified_target: bool,
        #[command(flatten)]
        src: SourceArgs,
    },
    /// Import a DMDE scan log (devscan.log) and rebuild its folders without a full rescan.
    ImportDmde {
        /// The DMDE log.
        log: PathBuf,
        #[arg(long)]
        source: String,
        #[arg(long)]
        session: PathBuf,
        /// Volume candidate number from the log (default: the one with most folders).
        #[arg(long)]
        volume: Option<u32>,
        /// Accept a log whose disk size differs from the source.
        #[arg(long)]
        force: bool,
        /// Also run a deep scan.
        #[arg(long)]
        deep: bool,
        #[arg(long)]
        codepage: Option<String>,
        #[arg(long)]
        allow_unverified_target: bool,
        #[command(flatten)]
        src: SourceArgs,
    },
    /// List volume candidates in a session.
    Volumes { session: PathBuf },
    /// List entries in a session.
    Ls {
        session: PathBuf,
        #[command(flatten)]
        select: SelectArgs,
        /// Include folders.
        #[arg(long)]
        dirs: bool,
        /// Show at most this many entries (0 = all).
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
    /// Copy entries to a folder on another disk, with a report.
    Recover {
        session: PathBuf,
        /// Target folder (must be on a different physical disk).
        #[arg(long)]
        to: PathBuf,
        /// Read from this source instead of the recorded one (e.g. an image of the disk).
        #[arg(long)]
        source: Option<String>,
        /// Use a source whose fingerprint does not match the session (marked in the report).
        #[arg(long)]
        force_rebind: bool,
        /// Allow a target on a disk Rescyo cannot identify (marked in the report).
        #[arg(long)]
        allow_unverified_target: bool,
        /// Skip the content check of recovered files.
        #[arg(long)]
        no_verify: bool,
        #[command(flatten)]
        select: SelectArgs,
        #[command(flatten)]
        src: SourceArgs,
    },
    /// Show a session's source, volumes and journal.
    Info { session: PathBuf },
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Self::Disks => "disks",
            Self::Scan { .. } => "scan",
            Self::ImportDmde { .. } => "import-dmde",
            Self::Volumes { .. } => "volumes",
            Self::Ls { .. } => "ls",
            Self::Recover { .. } => "recover",
            Self::Info { .. } => "info",
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut out = Out::new(
        cli.command.name(),
        cli.global.json,
        cli.global.json_stream,
        cli.global.quiet,
        cli.global.lang.as_deref(),
    );
    let cancel = CancelToken::new();
    {
        let c = cancel.clone();
        let mut hits = 0u32;
        let _ = ctrlc::set_handler(move || {
            hits += 1;
            if hits > 1 {
                std::process::exit(130);
            }
            c.cancel();
        });
    }
    let result = match &cli.command {
        Command::Disks => disks(&mut out),
        Command::Scan {
            source,
            session,
            deep,
            codepage,
            allow_unverified_target,
            src,
        } => scan(
            &mut out,
            source,
            session,
            *deep,
            codepage.as_deref(),
            *allow_unverified_target,
            src,
            &cancel,
        ),
        Command::ImportDmde {
            log,
            source,
            session,
            volume,
            force,
            deep,
            codepage,
            allow_unverified_target,
            src,
        } => import(
            &mut out,
            log,
            source,
            session,
            *volume,
            *force,
            *deep,
            codepage.as_deref(),
            *allow_unverified_target,
            src,
            &cancel,
        ),
        Command::Volumes { session } => volumes(&mut out, session),
        Command::Ls {
            session,
            select,
            dirs,
            limit,
        } => ls(&mut out, session, select, *dirs, *limit),
        Command::Recover {
            session,
            to,
            source,
            force_rebind,
            allow_unverified_target,
            no_verify,
            select,
            src,
        } => {
            let flags = RecoverFlags {
                force_rebind: *force_rebind,
                allow_unverified: *allow_unverified_target,
                verify: !*no_verify,
            };
            recover(
                &mut out,
                session,
                to,
                source.as_deref(),
                flags,
                select,
                src,
                &cancel,
            )
        }
        Command::Info { session } => info(&mut out, session),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(f) => ExitCode::from(out.fail(f)),
    }
}

fn policy(a: &SourceArgs) -> ReadPolicy {
    ReadPolicy {
        retries: a.retries,
        alarm_after_errors: (!a.continue_on_errors).then_some(16),
        ..ReadPolicy::default()
    }
}

fn is_device(path: &str) -> bool {
    if path.starts_with(r"\\.\") || path.starts_with(r"\\?\") {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if let Ok(m) = std::fs::metadata(path) {
            return m.file_type().is_block_device() || m.file_type().is_char_device();
        }
    }
    false
}

fn open_source(path: &str, a: &SourceArgs) -> Result<GuardedReader, Failure> {
    let src: Arc<dyn BlockSource> = if is_device(path) {
        Arc::new(PhysicalDisk::open(path).map_err(|e| Failure::source_open(path, &e))?)
    } else {
        Arc::new(
            ImageSource::open(Path::new(path), a.sector_size).map_err(|e| Failure::source_open(path, &e))?,
        )
    };
    Ok(GuardedReader::new(src, policy(a)))
}

fn codepage(name: Option<&str>) -> Result<Option<Codepage>, Failure> {
    match name {
        None => Ok(None),
        Some(n) => Codepage::from_name(n)
            .map(Some)
            .ok_or_else(|| Failure::usage(format!("unknown code page {n:?}"))),
    }
}

fn locale_codepage(lang: &str) -> Option<Codepage> {
    match lang {
        "ru" => Some(Codepage::Cp866),
        "ja" => Some(Codepage::Cp932),
        "zh-CN" => Some(Codepage::Cp936),
        "ko" => Some(Codepage::Cp949),
        _ => None,
    }
}

fn disks(out: &mut Out) -> Result<u8, Failure> {
    let list = rescyo_io::list_disks().map_err(|e| Failure::internal(&e.to_string()))?;
    let data = dto::DisksData {
        disks: list
            .into_iter()
            .map(|d| dto::Disk {
                path: d.path,
                size: d.size,
                logical_sector: d.logical_sector,
                model: d.model,
                serial: d.serial,
                removable: d.removable,
            })
            .collect(),
    };
    if !out.machine() {
        if data.disks.is_empty() {
            out.say(&out.t("disks-none", &[]));
        }
        for d in &data.disks {
            let sz = d.size.map_or_else(|| out.t("disks-size-unknown", &[]), size);
            let rem = if d.removable == Some(true) {
                format!("  [{}]", out.t("disks-removable", &[]))
            } else {
                String::new()
            };
            out.say(&format!(
                "{:<24} {:>12}  {}{rem}",
                d.path,
                sz,
                d.model.as_deref().unwrap_or("")
            ));
        }
    }
    out.done(&data);
    Ok(0)
}

/// Creates a new Session for `r` at `path`, refusing a file on the Source's own disk.
fn new_session(r: &GuardedReader, path: &Path, allow_unverified: bool) -> Result<Session, Failure> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    rescyo_core::check_destination(&SystemResolver, r.identity(), dir, allow_unverified)
        .map_err(|p| Failure::session_on_source(&p))?;
    let fp = SourceFingerprint::compute(r).map_err(Failure::from_guard)?;
    Session::create(path, r.identity(), &fp).map_err(|e| Failure::session(path, e))
}

fn scan_data(session: &Path, r: &rescyo_core::ScanReport) -> dto::ScanData {
    dto::ScanData {
        session: session.display().to_string(),
        partition_table: r.partition_table.into(),
        mode: match r.mode {
            ScanMode::Quick => "quick".into(),
            ScanMode::Deep => "deep".into(),
        },
        volumes: r
            .volumes
            .iter()
            .map(|v| dto::Volume {
                id: v.id,
                fs: v.fs.as_str().into(),
                offset: v.offset,
                cluster_size: v.cluster_size,
                data_offset: v.data_offset,
                score: v.score,
                walked: v.walked,
                files: v.files,
                dirs: v.dirs,
                deleted: v.deleted,
                lost_folders: v.lost_folders,
            })
            .collect(),
        unreadable_sectors: r.unreadable_sectors,
    }
}

fn print_scan(out: &Out, d: &dto::ScanData) {
    if out.machine() {
        return;
    }
    let walked = d.volumes.iter().filter(|v| v.walked).count();
    for v in &d.volumes {
        if v.walked {
            out.say(&out.t(
                "scan-volume",
                &[
                    ("id", v.id.into()),
                    ("fs", v.fs.clone().into()),
                    ("cluster", size(u64::from(v.cluster_size)).into()),
                    ("files", v.files.into()),
                    ("dirs", v.dirs.into()),
                    ("deleted", v.deleted.into()),
                    ("lost", v.lost_folders.into()),
                ],
            ));
        }
    }
    if walked == 0 {
        out.say(&out.t("scan-no-volumes", &[]));
    }
    if d.unreadable_sectors > 0 {
        out.say(&out.t("scan-bad-sectors", &[("count", d.unreadable_sectors.into())]));
    }
    out.say(&out.t("scan-done", &[("session", d.session.clone().into())]));
}

#[allow(clippy::too_many_arguments, reason = "one argument per command-line option")]
fn scan(
    out: &mut Out,
    source: &str,
    session: &Path,
    deep: bool,
    cp: Option<&str>,
    allow_unverified: bool,
    src: &SourceArgs,
    cancel: &CancelToken,
) -> Result<u8, Failure> {
    let cp = codepage(cp)?;
    let r = open_source(source, src)?;
    let mut s = new_session(&r, session, allow_unverified)?;
    out.say(&out.t("scan-start", &[("source", source.into())]));
    let opts = ScanOptions {
        mode: if deep { ScanMode::Deep } else { ScanMode::Quick },
        codepage: cp,
        locale_codepage: locale_codepage(out.lang()),
        ..ScanOptions::default()
    };
    let report =
        rescyo_core::scan(&r, &mut s, &opts, &mut |p| out.progress(p), cancel).map_err(Failure::from_scan)?;
    out.end_progress();
    let data = scan_data(session, &report);
    print_scan(out, &data);
    out.done(&data);
    Ok(0)
}

#[allow(clippy::too_many_arguments, reason = "one argument per command-line option")]
fn import(
    out: &mut Out,
    log: &Path,
    source: &str,
    session: &Path,
    volume: Option<u32>,
    force: bool,
    deep: bool,
    cp: Option<&str>,
    allow_unverified: bool,
    src: &SourceArgs,
    cancel: &CancelToken,
) -> Result<u8, Failure> {
    let cp = codepage(cp)?;
    let bytes = std::fs::read(log).map_err(|e| Failure::dmde_parse(&e.to_string()))?;
    let parsed = rescyo_dmde::parse(&bytes).map_err(|e| Failure::dmde_parse(&e.to_string()))?;
    let r = open_source(source, src)?;
    let mut s = new_session(&r, session, allow_unverified)?;
    let io = ImportOptions {
        volume,
        force,
        scan: ScanOptions {
            mode: if deep { ScanMode::Deep } else { ScanMode::Quick },
            codepage: cp,
            locale_codepage: locale_codepage(out.lang()),
            ..ScanOptions::default()
        },
    };
    let (summary, report) =
        rescyo_core::import_dmde(&r, &mut s, &parsed, &io, &mut |p| out.progress(p), cancel).map_err(
            |e| match e {
                ImportError::GeometryMismatch {
                    log_sectors,
                    log_sector_size,
                    sectors,
                    sector_size,
                } => Failure::dmde_geometry(
                    &format!("{log_sectors} × {log_sector_size}"),
                    &format!("{sectors} × {sector_size}"),
                ),
                ImportError::Scan(s) => Failure::from_scan(s),
                other => Failure::dmde_parse(&other.to_string()),
            },
        )?;
    out.end_progress();
    if !out.machine() {
        out.say(&out.t(
            "import-summary",
            &[
                ("index", u64::from(summary.volume_index).into()),
                ("cluster", size(u64::from(summary.cluster_size)).into()),
                ("offset", format!("{:#x}", summary.data_offset).into()),
                ("folders", summary.dmde_folders.into()),
                ("seeds", summary.seeds.into()),
                ("offgrid", summary.off_grid.into()),
            ],
        ));
    }
    let scan = scan_data(session, &report);
    print_scan(out, &scan);
    out.done(&dto::ImportData {
        dmde_volume: summary.volume_index,
        cluster_size: summary.cluster_size,
        data_offset: summary.data_offset,
        dmde_folders: summary.dmde_folders,
        seeds_used: summary.seeds,
        seeds_off_grid: summary.off_grid,
        warnings: summary.warnings,
        scan,
    });
    Ok(0)
}

fn open_session(path: &Path) -> Result<Session, Failure> {
    Session::open(path).map_err(|e| Failure::session(path, e))
}

fn volumes(out: &mut Out, session: &Path) -> Result<u8, Failure> {
    let s = open_session(session)?;
    let rows = s.volumes().map_err(|e| Failure::session(session, e))?;
    let data = dto::VolumesData {
        volumes: rows
            .into_iter()
            .map(|v| dto::VolumeInfo {
                id: v.id,
                fs: v.candidate.fs.as_str().into(),
                offset: v.candidate.offset,
                cluster_size: v.candidate.cluster_size,
                data_offset: v.candidate.data_offset,
                size: v.candidate.size,
                label: v.candidate.label.clone(),
                score: v.candidate.score,
                walked: v.walked,
                evidence: serde_json::to_value(&v.candidate.evidence).unwrap_or_default(),
            })
            .collect(),
    };
    if !out.machine() {
        for v in &data.volumes {
            out.say(&format!(
                "{:>3}  {:<6} {:>14}  cluster {:>8}  data {:#014x}  score {:>5}  {}{}",
                v.id,
                v.fs,
                v.offset.map_or_else(|| "?".to_owned(), |o| format!("{o:#x}")),
                size(u64::from(v.cluster_size)),
                v.data_offset,
                v.score,
                if v.walked { "walked" } else { "" },
                v.label
                    .as_deref()
                    .map(|l| format!("  \"{l}\""))
                    .unwrap_or_default()
            ));
        }
    }
    out.done(&data);
    Ok(0)
}

fn ls(out: &mut Out, session: &Path, select: &SelectArgs, dirs: bool, limit: usize) -> Result<u8, Failure> {
    let s = open_session(session)?;
    let sel = select.selection();
    let entries = s
        .entries(|e| dirs || e.entry.kind == EntryKind::File)
        .map_err(|e| Failure::session(session, e))?;
    let mut rows = Vec::new();
    for e in entries {
        let path = s.path_of(e.id).map_err(|x| Failure::session(session, x))?;
        if !sel.matches(&path, &e) {
            continue;
        }
        rows.push(dto::EntryRow {
            id: e.id.0,
            volume: e.volume,
            path,
            kind: match e.entry.kind {
                EntryKind::File => "file".into(),
                EntryKind::Directory => "directory".into(),
            },
            state: match e.entry.state {
                EntryState::Live => "live".into(),
                EntryState::Deleted => "deleted".into(),
            },
            size: e.entry.size,
            modified: e.entry.times.modified.map(|t| t.to_rfc3339()),
            assembly: e.entry.assembly.as_str().into(),
        });
    }
    let total = rows.len() as u64;
    if limit > 0 {
        rows.truncate(limit);
    }
    if !out.machine() {
        for r in &rows {
            let mark = if r.state == "deleted" { "x" } else { " " };
            let guess = if r.assembly == "contiguous_assumed" {
                "?"
            } else {
                " "
            };
            out.say(&format!(
                "{mark}{guess} {:>10}  {:<19}  {}",
                if r.kind == "directory" {
                    "<dir>".to_owned()
                } else {
                    size(r.size)
                },
                r.modified.as_deref().unwrap_or(""),
                r.path
            ));
        }
        out.say(&out.t("ls-total", &[("count", total.into())]));
    }
    out.done(&dto::LsData { entries: rows, total });
    Ok(0)
}

#[derive(Debug, Clone, Copy)]
struct RecoverFlags {
    force_rebind: bool,
    allow_unverified: bool,
    verify: bool,
}

#[allow(clippy::too_many_arguments, reason = "one argument per command-line option")]
fn recover(
    out: &mut Out,
    session: &Path,
    to: &Path,
    source: Option<&str>,
    flags: RecoverFlags,
    select: &SelectArgs,
    src_args: &SourceArgs,
    cancel: &CancelToken,
) -> Result<u8, Failure> {
    let RecoverFlags {
        force_rebind,
        allow_unverified,
        verify,
    } = flags;
    let s = open_session(session)?;
    let rec = s.source().map_err(|e| Failure::session(session, e))?;
    let path = source.map_or_else(|| rec.identity.path.clone(), str::to_owned);
    let r = open_source(&path, src_args)?;
    let fresh = SourceFingerprint::compute(&r).map_err(Failure::from_guard)?;
    match rec.fingerprint.compare(&fresh) {
        FingerprintMatch::Match => {
            if path != rec.identity.path {
                s.rebind(r.identity(), false)
                    .map_err(|e| Failure::session(session, e))?;
            }
        }
        m => {
            let reason = match m {
                FingerprintMatch::Mismatch(why) => why.to_owned(),
                _ => "nothing comparable could be read".to_owned(),
            };
            if !force_rebind {
                return Err(Failure::source_mismatch(&path, &reason));
            }
            s.rebind(r.identity(), true)
                .map_err(|e| Failure::session(session, e))?;
        }
    }
    let opts = RecoverOptions {
        target: to.to_path_buf(),
        selection: select.selection(),
        allow_unverified_target: allow_unverified,
        verify,
    };
    let plan = rescyo_core::plan(&s, &SystemResolver, &opts).map_err(Failure::from_recovery)?;
    if !plan.problems.is_empty() {
        return Err(Failure::preflight(&plan.problems, out));
    }
    out.say(&out.t(
        "recover-plan",
        &[
            ("files", (plan.items.len() as u64).into()),
            ("size", size(plan.bytes).into()),
            ("target", to.display().to_string().into()),
        ],
    ));
    let report = rescyo_core::run(&r, &s, &plan, &mut |p| out.recovery_progress(p), cancel)
        .map_err(Failure::from_recovery)?;
    out.end_progress();
    let t = &report.totals;
    let json_path = to.join(rescyo_core::recovery::report::JSON_NAME);
    if !out.machine() {
        out.say(&out.t(
            "recover-done",
            &[
                ("files", (t.files - t.failed).into()),
                ("size", size(t.bytes).into()),
                ("report", json_path.display().to_string().into()),
            ],
        ));
        for (id, n) in [
            ("recover-partial", t.partial),
            ("recover-guessed", t.guessed),
            ("recover-resumed", t.resumed),
            ("recover-failed", t.failed),
        ] {
            if n > 0 {
                out.say(&out.t(id, &[("count", Arg::from(n))]));
            }
        }
    }
    out.done(&dto::RecoverData {
        target: to.display().to_string(),
        report_json: json_path.display().to_string(),
        report_csv: to
            .join(rescyo_core::recovery::report::CSV_NAME)
            .display()
            .to_string(),
        target_unverified: report.target_unverified,
        source_unverified: report.source.unverified,
        totals: dto::Totals {
            files: t.files,
            bytes: t.bytes,
            intact: t.intact,
            partial: t.partial,
            damaged: t.damaged,
            unverified: t.unverified,
            guessed: t.guessed,
            resumed: t.resumed,
            failed: t.failed,
        },
    });
    Ok(if report.is_clean() { 0 } else { 5 })
}

fn info(out: &mut Out, session: &Path) -> Result<u8, Failure> {
    let s = open_session(session)?;
    let e = |x: SessionError| Failure::session(session, x);
    let src = s.source().map_err(e)?;
    let vols = s.volumes().map_err(e)?;
    let entries = s.entry_count().map_err(e)?;
    let bad = s.bad_map().map_err(e)?.bad_sectors();
    let journal = s.journal_entries().map_err(e)?;
    let data = dto::InfoData {
        session: session.display().to_string(),
        source_path: src.identity.path.clone(),
        source_kind: format!("{:?}", src.identity.kind).to_lowercase(),
        source_size: src.fingerprint.size,
        source_unverified: src.forced,
        volumes: vols.len() as u64,
        entries,
        unreadable_sectors: bad,
        journal: journal
            .into_iter()
            .map(|(time, action, detail)| dto::JournalRow { time, action, detail })
            .collect(),
    };
    if !out.machine() {
        out.say(&out.t("info-source", &[("path", data.source_path.clone().into())]));
        if data.source_unverified {
            out.say(&out.t("info-source-unverified", &[]));
        }
        out.say(&out.t("info-volumes", &[("count", data.volumes.into())]));
        out.say(&out.t("info-entries", &[("count", data.entries.into())]));
        if bad > 0 {
            out.say(&out.t("scan-bad-sectors", &[("count", bad.into())]));
        }
    }
    out.done(&data);
    Ok(0)
}

#[cfg(test)]
mod tests;
