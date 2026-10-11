//! Recovery: copy selected Entries to a Target on another device, after a full Preflight.
//!
//! The only code in Rescyo that writes user-visible files. It never writes to the Source's
//! devices (ADR-0003), never overwrites an existing file, and records every file in a Report.

pub mod names;
pub mod report;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, FileTimes};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rescyo_fs::{CancelToken, EntryKind, EntryState, Extent, Parent, TimeZone, Times, Timestamp};
use rescyo_io::device::{Placement, TargetInfo};
use rescyo_io::{GuardError, GuardedReader, SourceIdentity};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::session::{Session, SessionError, StoredEntry};
use names::{NameRules, Namer, sanitize};
use report::{FileRecord, Integrity, RecoveryReport, SourceInfo};

const CHUNK: usize = 1 << 20;

/// Answers where paths and Sources live. The system implementation queries the OS; tests use
/// a fake so they never touch the developer's disks (ADR-0002).
pub trait DeviceResolver {
    fn path(&self, p: &Path) -> Placement;
    fn source(&self, id: &SourceIdentity) -> Placement;
    fn target_info(&self, p: &Path) -> TargetInfo;
}

/// The operating system's view.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl DeviceResolver for SystemResolver {
    fn path(&self, p: &Path) -> Placement {
        rescyo_io::device::placement_of_path(p)
    }

    fn source(&self, id: &SourceIdentity) -> Placement {
        rescyo_io::device::placement_of_source(id)
    }

    fn target_info(&self, p: &Path) -> TargetInfo {
        rescyo_io::device::target_info(p)
    }
}

/// Which deleted Entries to include.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DeletedMode {
    #[default]
    Exclude,
    Include,
    Only,
}

/// Which Entries a command acts on.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub deleted: DeletedMode,
    /// Lower-case extensions without the dot; empty means all.
    pub extensions: Vec<String>,
    /// Only paths starting with this prefix (`/`-separated, as shown by `ls`).
    pub path_prefix: Option<String>,
    pub volume: Option<i64>,
}

impl Selection {
    pub fn matches(&self, path: &str, e: &StoredEntry) -> bool {
        let deleted = e.entry.state == EntryState::Deleted;
        let state_ok = match self.deleted {
            DeletedMode::Exclude => !deleted,
            DeletedMode::Include => true,
            DeletedMode::Only => deleted,
        };
        let ext_ok = self.extensions.is_empty()
            || names::split_ext(&e.entry.name)
                .1
                .strip_prefix('.')
                .is_some_and(|x| self.extensions.iter().any(|w| w.eq_ignore_ascii_case(x)));
        let prefix_ok = self
            .path_prefix
            .as_deref()
            .is_none_or(|p| path.starts_with(p.trim_start_matches('/')));
        let vol_ok = self.volume.is_none_or(|v| v == e.volume);
        state_ok && ext_ok && prefix_ok && vol_ok
    }
}

#[derive(Debug, Clone)]
pub struct RecoverOptions {
    pub target: PathBuf,
    pub selection: Selection,
    /// Accept a Target whose devices cannot be determined (recorded in the Report).
    pub allow_unverified_target: bool,
}

/// Something that stops a Recovery before any byte is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Problem {
    TargetOnSourceDevice { devices: Vec<String> },
    UnverifiedTarget { reason: String },
    SourceDevicesUnknown { reason: String },
    InsufficientSpace { needed: u64, free: u64 },
    FileTooLarge { path: String, size: u64, limit: u64 },
    NothingSelected,
}

#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("cannot write to the target: {0}")]
    Io(#[from] std::io::Error),
    #[error("preflight failed")]
    Blocked(Vec<Problem>),
    #[error("the source degraded: {errors} unreadable sectors")]
    Degraded { errors: u64 },
    #[error("the source disappeared: {0}")]
    Gone(std::io::Error),
    #[error("cancelled")]
    Cancelled,
}

/// One file to copy.
#[derive(Debug, Clone)]
pub struct PlanItem {
    pub entry: StoredEntry,
    pub source_path: String,
    /// Target path components relative to the Target root.
    pub target: Vec<String>,
    pub renamed: bool,
}

/// What a Recovery will do, decided before it writes anything.
#[derive(Debug, Clone)]
pub struct Plan {
    pub target: PathBuf,
    pub items: Vec<PlanItem>,
    /// Directories to create (with their Entry for timestamps), parents first.
    pub dirs: Vec<(Vec<String>, Times)>,
    pub bytes: u64,
    pub problems: Vec<Problem>,
    pub target_unverified: bool,
    pub target_info: TargetInfo,
}

/// Checks that `path` (a Target or a Session file) is on devices disjoint from the Source.
/// Returns whether the check was overridden.
pub fn check_destination(
    resolver: &dyn DeviceResolver,
    source: &SourceIdentity,
    path: &Path,
    allow_unverified: bool,
) -> std::result::Result<bool, Problem> {
    let src = match resolver.source(source) {
        Placement::Local(d) => d,
        Placement::Remote(_) => Default::default(),
        Placement::Unknown(reason) => {
            return if allow_unverified {
                Ok(true)
            } else {
                Err(Problem::SourceDevicesUnknown { reason })
            };
        }
    };
    match resolver.path(path) {
        Placement::Local(d) => {
            let shared: Vec<String> = d.intersection(&src).cloned().collect();
            if shared.is_empty() {
                Ok(false)
            } else {
                Err(Problem::TargetOnSourceDevice { devices: shared })
            }
        }
        Placement::Remote(_) => Ok(false),
        Placement::Unknown(reason) => {
            if allow_unverified {
                Ok(true)
            } else {
                Err(Problem::UnverifiedTarget { reason })
            }
        }
    }
}

/// Builds the Recovery plan and runs the Preflight. Nothing is written.
pub fn plan(
    session: &Session,
    resolver: &dyn DeviceResolver,
    opts: &RecoverOptions,
) -> Result<Plan, RecoveryError> {
    let source = session.source()?.identity;
    let mut problems = Vec::new();
    let target_unverified =
        match check_destination(resolver, &source, &opts.target, opts.allow_unverified_target) {
            Ok(v) => v,
            Err(p) => {
                problems.push(p);
                false
            }
        };
    let info = resolver.target_info(&opts.target);
    let rules = NameRules::for_target(info.windows_names(), info.case_insensitive());

    let all = session.entries(|_| true)?;
    let by_id: HashMap<u64, &StoredEntry> = all.iter().map(|e| (e.id.0, e)).collect();
    let walked: Vec<i64> = session
        .volumes()?
        .iter()
        .filter(|v| v.walked)
        .map(|v| v.id)
        .collect();
    let multi = walked.len() > 1;
    let vol_names: HashMap<i64, String> = session
        .volumes()?
        .iter()
        .map(|v| (v.id, format!("Volume_{}_{}", v.id, v.candidate.fs.as_str())))
        .collect();

    let mut namer = Namer::default();
    // Assigned Target names: per directory Entry, and per synthetic folder.
    let mut dir_names: HashMap<u64, String> = HashMap::new();
    let mut synthetic: HashMap<(String, String), String> = HashMap::new();
    let mut dirs: BTreeMap<Vec<String>, Times> = BTreeMap::new();
    let mut items = Vec::new();
    let mut bytes = 0u64;
    let limit = info.max_file_size();

    for e in all.iter().filter(|e| e.entry.kind == EntryKind::File) {
        // Source path: names from the root, with the lost-folder prefix.
        let mut chain: Vec<&StoredEntry> = vec![e];
        let mut lost = None;
        let mut cur = e;
        for _ in 0..4096 {
            match cur.entry.parent {
                Parent::Entry(p) => match by_id.get(&p.0) {
                    Some(pe) => {
                        chain.push(pe);
                        cur = pe;
                    }
                    None => break,
                },
                Parent::Lost { parent_ref } => {
                    lost = Some(parent_ref);
                    break;
                }
                Parent::Root => break,
            }
        }
        chain.reverse();
        let mut src_parts: Vec<String> = Vec::new();
        if let Some(l) = lost {
            src_parts.push("_LOST_FOLDERS".into());
            src_parts.push(format!("parent_{l}"));
        }
        src_parts.extend(chain.iter().map(|c| c.entry.name.clone()));
        let source_path = src_parts.join("/");
        if !opts.selection.matches(&source_path, e) {
            continue;
        }

        // Target components, each sanitized and de-duplicated in its directory.
        let mut target: Vec<String> = Vec::new();
        let mut renamed = false;
        let mut synth = |target: &mut Vec<String>, name: String, namer: &mut Namer| {
            let dir = target.join("/");
            let key = (dir.clone(), name.clone());
            let assigned = synthetic
                .entry(key)
                .or_insert_with(|| namer.reserve(&dir, &sanitize(&name, rules), rules))
                .clone();
            target.push(assigned);
        };
        if multi && let Some(v) = vol_names.get(&e.volume) {
            synth(&mut target, v.clone(), &mut namer);
        }
        if let Some(l) = lost {
            synth(&mut target, "_LOST_FOLDERS".into(), &mut namer);
            synth(&mut target, format!("parent_{l}"), &mut namer);
        }
        for c in chain.iter().take(chain.len() - 1) {
            let dir = target.join("/");
            let name = dir_names
                .entry(c.id.0)
                .or_insert_with(|| namer.reserve(&dir, &sanitize(&c.entry.name, rules), rules))
                .clone();
            renamed |= name != c.entry.name;
            target.push(name);
            dirs.entry(target.clone()).or_insert(c.entry.times);
        }
        let dir = target.join("/");
        let name = namer.reserve(&dir, &sanitize(&e.entry.name, rules), rules);
        renamed |= name != e.entry.name;
        target.push(name);
        for k in 1..target.len() {
            dirs.entry(target.get(..k).unwrap_or_default().to_vec())
                .or_default();
        }
        if let Some(l) = limit
            && e.entry.size > l
        {
            problems.push(Problem::FileTooLarge {
                path: source_path.clone(),
                size: e.entry.size,
                limit: l,
            });
        }
        bytes += e.entry.size;
        items.push(PlanItem {
            entry: e.clone(),
            source_path,
            target,
            renamed,
        });
    }
    if items.is_empty() {
        problems.push(Problem::NothingSelected);
    }
    // Room for directory metadata and the reports.
    let needed = bytes + bytes / 100 + (1 << 20);
    if let Some(free) = info.free_bytes
        && free < needed
    {
        problems.push(Problem::InsufficientSpace { needed, free });
    }
    Ok(Plan {
        target: opts.target.clone(),
        items,
        dirs: dirs.into_iter().collect(),
        bytes,
        problems,
        target_unverified,
        target_info: info,
    })
}

/// Progress of a running Recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RecoveryProgress {
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// Copies the plan's files. Refuses to start if the Preflight found problems.
pub fn run(
    r: &GuardedReader,
    session: &Session,
    plan: &Plan,
    progress: &mut dyn FnMut(RecoveryProgress),
    cancel: &CancelToken,
) -> Result<RecoveryReport, RecoveryError> {
    if !plan.problems.is_empty() {
        return Err(RecoveryError::Blocked(plan.problems.clone()));
    }
    let src = session.source()?;
    let started = now();
    let root = long_path(&plan.target)?;
    std::fs::create_dir_all(&root)?;
    let prior = report::RecoveryReport::read_json(&root.join(report::JSON_NAME));
    let prior: HashMap<String, (u64, String)> = prior
        .map(|p| {
            p.files
                .into_iter()
                .filter_map(|f| Some((f.target_path, (f.size, f.sha256?))))
                .collect()
        })
        .unwrap_or_default();
    session.journal(
        "recovery_started",
        &serde_json::json!({ "target": plan.target, "files": plan.items.len() }),
    )?;

    for (d, _) in &plan.dirs {
        std::fs::create_dir_all(join(&root, d))?;
    }
    let mut report = RecoveryReport {
        schema_version: report::REPORT_SCHEMA_VERSION,
        tool_version: env!("CARGO_PKG_VERSION").into(),
        session: session.path().display().to_string(),
        source: SourceInfo {
            path: src.identity.path.clone(),
            unverified: src.forced,
        },
        target: plan.target.display().to_string(),
        target_unverified: plan.target_unverified,
        started,
        finished: 0,
        totals: Default::default(),
        files: Vec::new(),
    };
    let mut p = RecoveryProgress {
        files_done: 0,
        files_total: plan.items.len() as u64,
        bytes_done: 0,
        bytes_total: plan.bytes,
    };
    let mut taken: HashSet<PathBuf> = HashSet::new();
    let mut outcome = Ok(());
    for item in &plan.items {
        if cancel.is_cancelled() {
            outcome = Err(RecoveryError::Cancelled);
            break;
        }
        let rel = item.target.join("/");
        let mut path = join(&root, &item.target);
        let mut record = FileRecord {
            source_path: item.source_path.clone(),
            target_path: rel.clone(),
            size: item.entry.entry.size,
            state: item.entry.entry.state,
            assembly: item.entry.entry.assembly,
            integrity: Integrity::Unverified,
            sha256: None,
            unreadable: Vec::new(),
            renamed: item.renamed,
            resumed: false,
            error: None,
        };
        if path.exists() {
            // Resume: the same file from an earlier run, verified by its hash.
            if let Some((size, sha)) = prior.get(&rel)
                && *size == item.entry.entry.size
                && hash_file(&path).ok().as_ref() == Some(sha)
            {
                record.sha256 = Some(sha.clone());
                record.resumed = true;
                p.files_done += 1;
                p.bytes_done += item.entry.entry.size;
                report.files.push(record);
                progress(p);
                continue;
            }
            path = free_name(&path, &taken);
            record.target_path = path
                .strip_prefix(&root)
                .map(|x| x.to_string_lossy().replace('\\', "/"))
                .unwrap_or(rel);
            record.renamed = true;
        }
        taken.insert(path.clone());
        match copy_one(r, &item.entry, &path, &mut |n| {
            p.bytes_done += n;
            progress(p);
        }) {
            Ok((sha, unreadable)) => {
                record.sha256 = Some(sha);
                record.integrity = if unreadable.is_empty() {
                    Integrity::Unverified
                } else {
                    Integrity::Partial
                };
                record.unreadable = unreadable;
            }
            Err(CopyError::Guard(GuardError::DegradationAlarm { errors })) => {
                record.error = Some("source degraded".into());
                report.files.push(record);
                outcome = Err(RecoveryError::Degraded { errors });
                break;
            }
            Err(CopyError::Guard(GuardError::Gone(e))) => {
                record.error = Some("source disappeared".into());
                report.files.push(record);
                outcome = Err(RecoveryError::Gone(e));
                break;
            }
            Err(e) => record.error = Some(e.to_string()),
        }
        p.files_done += 1;
        progress(p);
        report.files.push(record);
    }
    // Directory times last: writing files inside a directory changes its mtime.
    for (d, times) in plan.dirs.iter().rev() {
        let _ = set_times(&join(&root, d), times, true);
    }
    report.finished = now();
    report.tally();
    report.write_json(&root.join(report::JSON_NAME))?;
    report.write_csv(&root.join(report::CSV_NAME))?;
    session.journal(
        "recovery_finished",
        &serde_json::to_value(&report.totals).unwrap_or_default(),
    )?;
    outcome.map(|()| report)
}

#[derive(Debug, thiserror::Error)]
enum CopyError {
    #[error(transparent)]
    Guard(GuardError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Copies one Entry, hashing as it goes. Returns the SHA-256 and the unreadable ranges.
fn copy_one(
    r: &GuardedReader,
    e: &StoredEntry,
    path: &Path,
    progress: &mut dyn FnMut(u64),
) -> Result<(String, Vec<[u64; 2]>), CopyError> {
    let mut out = File::create_new(path)?;
    let mut hasher = Sha256::new();
    let mut unreadable: Vec<[u64; 2]> = Vec::new();
    let mut pos = 0u64;
    let size = e.entry.size;
    let mut buf = vec![0u8; CHUNK];
    let put = |data: &[u8], out: &mut File, hasher: &mut Sha256, pos: &mut u64| -> std::io::Result<()> {
        out.write_all(data)?;
        hasher.update(data);
        *pos += data.len() as u64;
        Ok(())
    };
    for x in &e.entry.layout.extents {
        if pos >= size {
            break;
        }
        match x {
            Extent::Disk { offset, len } => {
                let mut done = 0u64;
                let len = (*len).min(size - pos);
                while done < len {
                    let n = usize::try_from((len - done).min(CHUNK as u64)).unwrap_or(CHUNK);
                    let chunk = buf.get_mut(..n).unwrap_or_default();
                    let at = offset + done;
                    match r.read(at, chunk) {
                        Ok(rep) => {
                            for u in rep.unreadable {
                                push_range(&mut unreadable, pos + (u.start - at), u.end - u.start);
                            }
                        }
                        Err(GuardError::OutOfBounds) => {
                            chunk.fill(0);
                            push_range(&mut unreadable, pos, n as u64);
                        }
                        Err(g) => return Err(CopyError::Guard(g)),
                    }
                    put(chunk, &mut out, &mut hasher, &mut pos)?;
                    done += n as u64;
                    progress(n as u64);
                }
            }
            Extent::Zero { len } => {
                let mut left = (*len).min(size - pos);
                buf.fill(0);
                while left > 0 {
                    let n = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
                    put(buf.get(..n).unwrap_or_default(), &mut out, &mut hasher, &mut pos)?;
                    left -= n as u64;
                }
            }
            Extent::Inline { data } => {
                let n = usize::try_from((data.len() as u64).min(size - pos)).unwrap_or(0);
                put(data.get(..n).unwrap_or_default(), &mut out, &mut hasher, &mut pos)?;
            }
        }
    }
    // A layout shorter than the size (lost allocation) is padded and reported.
    if pos < size {
        let missing = size - pos;
        push_range(&mut unreadable, pos, missing);
        buf.fill(0);
        let mut left = missing;
        while left > 0 {
            let n = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
            put(buf.get(..n).unwrap_or_default(), &mut out, &mut hasher, &mut pos)?;
            left -= n as u64;
        }
    }
    out.flush()?;
    let _ = apply_times(&out, &e.entry.times);
    out.sync_all()?;
    Ok((
        hasher.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        unreadable,
    ))
}

fn push_range(v: &mut Vec<[u64; 2]>, start: u64, len: u64) {
    if len == 0 {
        return;
    }
    if let Some(last) = v.last_mut()
        && last[0] + last[1] == start
    {
        last[1] += len;
        return;
    }
    v.push([start, len]);
}

fn hash_file(p: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n).unwrap_or_default());
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// `name (2).ext`, `name (3).ext`… that exists neither on disk nor among names taken in this run.
fn free_name(p: &Path, taken: &HashSet<PathBuf>) -> PathBuf {
    let parent = p.parent().unwrap_or(Path::new(""));
    let file = p
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = names::split_ext(&file);
    for i in 2u32.. {
        let cand = parent.join(format!("{stem} ({i}){ext}"));
        if !cand.exists() && !taken.contains(&cand) {
            return cand;
        }
    }
    unreachable!("u32 suffixes exhausted")
}

fn join(root: &Path, parts: &[String]) -> PathBuf {
    parts.iter().fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Absolute path; on Windows in `\\?\` form so paths longer than 260 characters work.
fn long_path(p: &Path) -> std::io::Result<PathBuf> {
    let abs = std::path::absolute(p)?;
    #[cfg(windows)]
    {
        let s = abs.to_string_lossy();
        if s.starts_with(r"\\?\") {
            return Ok(abs);
        }
        if let Some(unc) = s.strip_prefix(r"\\") {
            return Ok(PathBuf::from(format!(r"\\?\UNC\{unc}")));
        }
        return Ok(PathBuf::from(format!(r"\\?\{s}")));
    }
    #[cfg(not(windows))]
    Ok(abs)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Converts a stored timestamp to the system clock. Local-zone stamps (FAT) are interpreted in
/// this computer's time zone, as the OS that wrote them did.
fn system_time(t: &Timestamp) -> Option<SystemTime> {
    let unix = match t.zone {
        TimeZone::Utc => t.unix,
        TimeZone::Local => {
            use chrono::TimeZone as _;
            let naive = chrono::DateTime::from_timestamp(t.unix, 0)?.naive_utc();
            chrono::Local.from_local_datetime(&naive).earliest()?.timestamp()
        }
    };
    let base = if unix >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(u64::try_from(unix).ok()?))?
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(unix.unsigned_abs()))?
    };
    base.checked_add(Duration::from_nanos(u64::from(t.nanos)))
}

fn apply_times(f: &File, t: &Times) -> std::io::Result<()> {
    let mut ft = FileTimes::new();
    if let Some(m) = t.modified.as_ref().and_then(system_time) {
        ft = ft.set_modified(m);
    }
    if let Some(a) = t.accessed.as_ref().and_then(system_time) {
        ft = ft.set_accessed(a);
    }
    #[cfg(windows)]
    if let Some(c) = t.created.as_ref().and_then(system_time) {
        use std::os::windows::fs::FileTimesExt;
        ft = ft.set_created(c);
    }
    #[cfg(target_os = "macos")]
    if let Some(c) = t.created.as_ref().and_then(system_time) {
        use std::os::darwin::fs::FileTimesExt;
        ft = ft.set_created(c);
    }
    f.set_times(ft)
}

fn set_times(p: &Path, t: &Times, dir: bool) -> std::io::Result<()> {
    if t.modified.is_none() && t.accessed.is_none() && t.created.is_none() {
        return Ok(());
    }
    #[cfg(windows)]
    let f = {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_WRITE_ATTRIBUTES, and FILE_FLAG_BACKUP_SEMANTICS to open a directory.
        std::fs::OpenOptions::new()
            .access_mode(0x100)
            .custom_flags(if dir { 0x0200_0000 } else { 0 })
            .open(p)?
    };
    #[cfg(not(windows))]
    let f = {
        let _ = dir;
        File::open(p)?
    };
    apply_times(&f, t)
}

#[cfg(test)]
mod tests;
