//! Drives the window: sources in the sidebar, the four steps, and the long jobs (scan,
//! recovery, imaging) on worker threads that report back through the Slint event loop.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use rescyo_core::browse::{Model, Node, TreeRow, assembly_chip, strip_cells};
use rescyo_core::recovery::report::Integrity;
use rescyo_core::{
    DeletedMode, Problem, RecoverOptions, ScanMode, ScanOptions, Selection, Session, SystemResolver,
};
use rescyo_fs::{CancelToken, EntryKind, EntryState, Extent, Phase};
use rescyo_i18n::{Arg, Localizer, size};
use rescyo_io::{
    BlockSource, DiskInfo, GuardedReader, ImageSource, PhysicalDisk, ReadPolicy, SourceFingerprint,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};

use crate::{Count, Fact, FileRow, Kv, MainWindow, SourceItem, TreeItem};

const STRIP_CELLS: u64 = 220;

/// What the sidebar lists.
#[derive(Debug, Clone)]
enum Item {
    Disk(DiskInfo, bool),
    Image(PathBuf),
    Session(PathBuf),
}

/// What the path sheet is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    Target,
    OpenImage,
    OpenSession,
    SessionFile,
    ImageOut,
}

#[derive(Default)]
struct State {
    items: Vec<Item>,
    current: Option<usize>,
    session_path: Option<PathBuf>,
    session: Option<Session>,
    model: Option<Model>,
    rows: Vec<TreeRow>,
    file_ids: Vec<u64>,
    target: Option<PathBuf>,
    integrity: HashMap<u64, Integrity>,
    new_session: Option<PathBuf>,
    job: Option<CancelToken>,
    ask: Option<Ask>,
    source_size: u64,
    bad: Vec<std::ops::Range<u64>>,
    after_job: Option<usize>,
}

pub struct Controller {
    ui: Weak<MainWindow>,
    l: Rc<Localizer>,
    st: Rc<RefCell<State>>,
}

fn sh(s: impl Into<SharedString>) -> SharedString {
    s.into()
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

/// Default place for new session and image files: `~/Rescyo`.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Rescyo")
}

fn stamp() -> String {
    chrono_like_now()
}

/// `YYYY-MM-DD-HHMM` in UTC, without a date library.
fn chrono_like_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let (y, m, d) = civil_from_days(days);
    let rem = secs % 86_400;
    format!("{y:04}-{m:02}-{d:02}-{:02}{:02}", rem / 3600, rem % 3600 / 60)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Opens a disk or image for reading.
pub fn open_source(path: &str) -> Result<GuardedReader, String> {
    let device = path.starts_with(r"\\.\") || {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            std::fs::metadata(path)
                .is_ok_and(|m| m.file_type().is_block_device() || m.file_type().is_char_device())
        }
        #[cfg(not(unix))]
        {
            false
        }
    };
    let src: Arc<dyn BlockSource> = if device {
        Arc::new(PhysicalDisk::open(path).map_err(|e| e.to_string())?)
    } else {
        match rescyo_io::open_image(Path::new(path), 512, None) {
            Ok((s, _)) => s,
            Err(_) => Arc::new(ImageSource::open(Path::new(path), 512).map_err(|e| e.to_string())?),
        }
    };
    Ok(GuardedReader::new(src, ReadPolicy::default()))
}

/// Whether a disk holds the running system (never offered as a target, marked in the sidebar).
fn is_system_disk(d: &DiskInfo) -> bool {
    let root = if cfg!(windows) {
        PathBuf::from("C:\\")
    } else {
        PathBuf::from("/")
    };
    let id = rescyo_io::SourceIdentity {
        kind: rescyo_io::SourceKind::PhysicalDisk,
        path: d.path.clone(),
        model: None,
        serial: None,
    };
    match (
        rescyo_io::device::placement_of_path(&root),
        rescyo_io::device::placement_of_source(&id),
    ) {
        (rescyo_io::device::Placement::Local(a), rescyo_io::device::Placement::Local(b)) => {
            !a.is_disjoint(&b)
        }
        _ => false,
    }
}

impl Controller {
    pub fn new(ui: &MainWindow, l: Localizer) -> Rc<Self> {
        let c = Rc::new(Self {
            ui: ui.as_weak(),
            l: Rc::new(l),
            st: Rc::new(RefCell::new(State::default())),
        });
        crate::strings::fill(ui, &c.l);
        c.wire(ui);
        c
    }

    fn t(&self, id: &str, args: &[(&str, Arg<'_>)]) -> String {
        self.l.t(id, args)
    }

    fn ui(&self) -> Option<MainWindow> {
        self.ui.upgrade()
    }

    fn wire(self: &Rc<Self>, ui: &MainWindow) {
        macro_rules! on {
            ($setter:ident, |$c:ident $(, $a:ident : $t:ty)*| $body:expr) => {{
                let me = Rc::downgrade(self);
                ui.$setter(move |$($a: $t),*| {
                    if let Some($c) = me.upgrade() {
                        $body;
                    }
                });
            }};
        }
        on!(on_source_picked, |c, i: i32| c
            .pick(usize::try_from(i).unwrap_or(0)));
        on!(on_step_picked, |c, i: i32| c.step(i));
        on!(on_open_image, |c| c.ask(Ask::OpenImage, String::new()));
        on!(on_open_session, |c| c.ask(Ask::OpenSession, String::new()));
        on!(on_choose_session, |c| {
            let cur =
                c.st.borrow()
                    .new_session
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
            c.ask(Ask::SessionFile, cur)
        });
        on!(on_choose_target, |c| {
            let cur =
                c.st.borrow()
                    .target
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
            c.ask(Ask::Target, cur)
        });
        on!(on_image_first, |c| {
            let name = c.current_name();
            let p = home_dir().join(format!("{}-{}.img", sanitize(&name), stamp()));
            c.ask(Ask::ImageOut, p.display().to_string())
        });
        on!(on_scan, |c| c.scan());
        on!(on_stop, |c| {
            if let Some(j) = &c.st.borrow().job {
                j.cancel();
            }
        });
        on!(on_progress_primary_clicked, |c| c.after_job());
        on!(on_tree_picked, |c, _i: i32| c.refresh_files());
        on!(on_tree_toggled, |c, i: i32| c
            .toggle(usize::try_from(i).unwrap_or(0)));
        on!(on_file_picked, |c, _i: i32| c.refresh_inspector());
        on!(on_file_marked, |c, i: i32| c
            .mark(usize::try_from(i).unwrap_or(0)));
        on!(on_search_edited, |c, _q: SharedString| c.refresh_files());
        on!(on_recover, |c| c.recover());
        on!(on_sheet_ok, |c| c.sheet_ok());
        on!(on_sheet_cancel, |c| {
            c.st.borrow_mut().ask = None;
            if let Some(ui) = c.ui() {
                ui.set_sheet_visible(false);
            }
        });
    }

    /// Lists disks (when the system lets us), then any images and sessions opened earlier.
    pub fn load_sources(&self) {
        // RESCYO_NO_DISKS=1 leaves the machine's disks out (tests, demonstrations).
        let disks = if std::env::var_os("RESCYO_NO_DISKS").is_some() {
            Vec::new()
        } else {
            rescyo_io::list_disks().unwrap_or_default()
        };
        let mut st = self.st.borrow_mut();
        st.items.retain(|i| !matches!(i, Item::Disk(..)));
        let mut items: Vec<Item> = disks
            .into_iter()
            .map(|d| {
                let sys = is_system_disk(&d);
                Item::Disk(d, sys)
            })
            .collect();
        items.append(&mut st.items);
        st.items = items;
        drop(st);
        self.push_sources();
    }

    fn push_sources(&self) {
        let Some(ui) = self.ui() else { return };
        let st = self.st.borrow();
        let rows: Vec<SourceItem> = st
            .items
            .iter()
            .map(|it| match it {
                Item::Disk(d, sys) => {
                    let name = d.model.clone().unwrap_or_else(|| d.path.clone());
                    let mut detail = vec![d.size.map(size).unwrap_or_default()];
                    if d.removable == Some(true) {
                        detail.push(self.t("gui-removable", &[]));
                    }
                    if *sys {
                        detail.push(self.t("gui-protected", &[]));
                    }
                    SourceItem {
                        name: sh(name),
                        detail: sh(detail
                            .into_iter()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" · ")),
                        kind: if *sys { 1 } else { 0 },
                        group: 0,
                        locked: *sys,
                    }
                }
                Item::Image(p) => SourceItem {
                    name: sh(file_name(p)),
                    detail: sh(std::fs::metadata(p).map(|m| size(m.len())).unwrap_or_default()),
                    kind: 2,
                    group: 1,
                    locked: false,
                },
                Item::Session(p) => SourceItem {
                    name: sh(file_name(p)),
                    detail: sh(p.parent().map(|d| d.display().to_string()).unwrap_or_default()),
                    kind: 3,
                    group: 2,
                    locked: false,
                },
            })
            .collect();
        let groups: Vec<bool> = (0..3).map(|g| rows.iter().any(|r| r.group == g)).collect();
        ui.set_sources(model(rows));
        ui.set_source_groups(model(groups));
        ui.set_source_index(st.current.and_then(|i| i32::try_from(i).ok()).unwrap_or(-1));
    }

    fn current_name(&self) -> String {
        let st = self.st.borrow();
        match st.current.and_then(|i| st.items.get(i)) {
            Some(Item::Disk(d, _)) => d.model.clone().unwrap_or_else(|| d.path.clone()),
            Some(Item::Image(p) | Item::Session(p)) => file_name(p),
            None => "disk".into(),
        }
    }

    fn pick(&self, i: usize) {
        let item = self.st.borrow().items.get(i).cloned();
        self.st.borrow_mut().current = Some(i);
        match item {
            Some(Item::Session(p)) => self.open_session(&p),
            Some(Item::Disk(d, sys)) => {
                self.show_source(&d.path, d.model.clone(), d.size, d.serial.clone(), sys, true)
            }
            Some(Item::Image(p)) => {
                let len = std::fs::metadata(&p).map(|m| m.len()).ok();
                self.show_source(
                    &p.display().to_string(),
                    Some(file_name(&p)),
                    len,
                    None,
                    false,
                    false,
                );
            }
            None => {}
        }
    }

    fn show_source(
        &self,
        path: &str,
        name: Option<String>,
        len: Option<u64>,
        serial: Option<String>,
        system: bool,
        disk: bool,
    ) {
        let Some(ui) = self.ui() else { return };
        let name = name.unwrap_or_else(|| path.to_owned());
        let session = home_dir().join(format!("{}-{}.rescyo", sanitize(&name), stamp()));
        self.st.borrow_mut().new_session = Some(session.clone());
        let mut facts = vec![Fact {
            label: sh(self.t("gui-fact-path", &[])),
            value: sh(path),
        }];
        if let Some(n) = len {
            facts.push(Fact {
                label: sh(self.t("gui-fact-size", &[])),
                value: sh(size(n)),
            });
        }
        if let Some(s) = serial {
            facts.push(Fact {
                label: sh(self.t("gui-fact-serial", &[])),
                value: sh(s),
            });
        }
        ui.set_source_heading(sh(name));
        ui.set_source_facts(model(facts));
        ui.set_source_warning(sh(if system {
            self.t("gui-system-disk", &[])
        } else {
            String::new()
        }));
        ui.set_session_path(sh(session.display().to_string()));
        ui.set_can_image(disk);
        ui.set_step(0);
    }

    fn step(&self, i: i32) {
        let Some(ui) = self.ui() else { return };
        let has = self.st.borrow().session.is_some();
        let busy = self.st.borrow().job.is_some();
        match i {
            0 if !busy => {
                if let Some(c) = self.st.borrow().current {
                    drop(ui);
                    self.pick(c);
                }
            }
            2 if has && !busy => ui.set_step(2),
            _ => {}
        }
    }

    fn ask(&self, what: Ask, current: String) {
        let Some(ui) = self.ui() else { return };
        let (h, hint) = match what {
            Ask::Target => ("gui-sheet-target", "gui-sheet-target-hint"),
            Ask::OpenImage => ("gui-sheet-image", "gui-sheet-image-hint"),
            Ask::OpenSession => ("gui-sheet-session", "gui-sheet-session-hint"),
            Ask::SessionFile => ("gui-sheet-session-file", "gui-sheet-session-file-hint"),
            Ask::ImageOut => ("gui-sheet-image-out", "gui-sheet-image-out-hint"),
        };
        self.st.borrow_mut().ask = Some(what);
        ui.set_sheet_heading(sh(self.t(h, &[])));
        ui.set_sheet_hint(sh(self.t(hint, &[])));
        ui.set_sheet_text(sh(current));
        ui.set_sheet_visible(true);
    }

    fn sheet_ok(self: &Rc<Self>) {
        let Some(ui) = self.ui() else { return };
        let text = ui.get_sheet_text().trim().to_owned();
        let what = self.st.borrow_mut().ask.take();
        ui.set_sheet_visible(false);
        if text.is_empty() {
            return;
        }
        let p = PathBuf::from(&text);
        match what {
            Some(Ask::Target) => {
                self.st.borrow_mut().target = Some(p);
                self.refresh_preflight();
            }
            Some(Ask::OpenImage) => {
                let mut st = self.st.borrow_mut();
                st.items.push(Item::Image(p));
                let i = st.items.len() - 1;
                drop(st);
                self.push_sources();
                self.pick(i);
            }
            Some(Ask::OpenSession) => {
                let mut st = self.st.borrow_mut();
                st.items.push(Item::Session(p));
                let i = st.items.len() - 1;
                st.current = Some(i);
                drop(st);
                self.push_sources();
                self.pick(i);
            }
            Some(Ask::SessionFile) => {
                self.st.borrow_mut().new_session = Some(p);
                ui.set_session_path(sh(text));
            }
            Some(Ask::ImageOut) => self.image(p),
            None => {}
        }
    }

    fn source_path(&self) -> Option<String> {
        let st = self.st.borrow();
        match st.current.and_then(|i| st.items.get(i))? {
            Item::Disk(d, _) => Some(d.path.clone()),
            Item::Image(p) => Some(p.display().to_string()),
            Item::Session(_) => None,
        }
    }

    // ---------------------------------------------------------------- scan

    fn scan(self: &Rc<Self>) {
        let Some(ui) = self.ui() else { return };
        let Some(path) = self.source_path() else { return };
        let Some(session) = self.st.borrow().new_session.clone() else {
            return;
        };
        let deep = ui.get_deep();
        let carve = ui.get_carve() && deep;
        let cancel = CancelToken::new();
        self.st.borrow_mut().job = Some(cancel.clone());
        let name = self.current_name();
        self.progress_start(
            1,
            &self.t("gui-scanning", &[("source", Arg::from(name.as_str()))]),
        );
        let weak = self.ui.clone();
        let lang = self.l.lang();
        std::thread::spawn(move || {
            let l = Localizer::new(Some(lang));
            let result = scan_job(&path, &session, deep, carve, &cancel, &weak, &l);
            let session_for_ui = session.clone();
            let _ = slint::invoke_from_event_loop(move || {
                CONTROLLER.with(|c| {
                    if let Some(c) = c.borrow().as_ref() {
                        c.scan_done(result, &session_for_ui);
                    }
                });
            });
        });
    }

    fn scan_done(&self, result: Result<(), String>, session: &Path) {
        self.st.borrow_mut().job = None;
        match result {
            Ok(()) => {
                let mut st = self.st.borrow_mut();
                st.items.push(Item::Session(session.to_path_buf()));
                let i = st.items.len() - 1;
                st.current = Some(i);
                drop(st);
                self.push_sources();
                self.open_session(session);
            }
            Err(e) => self.progress_fail(&e),
        }
    }

    // ---------------------------------------------------------------- imaging

    fn image(self: &Rc<Self>, out: PathBuf) {
        let Some(path) = self.source_path() else { return };
        let cancel = CancelToken::new();
        self.st.borrow_mut().job = Some(cancel.clone());
        let name = self.current_name();
        self.progress_start(0, &self.t("gui-imaging", &[("source", Arg::from(name.as_str()))]));
        let weak = self.ui.clone();
        let lang = self.l.lang();
        std::thread::spawn(move || {
            let l = Localizer::new(Some(lang));
            let result = image_job(&path, &out, &cancel, &weak, &l);
            let _ = slint::invoke_from_event_loop(move || {
                CONTROLLER.with(|c| {
                    if let Some(c) = c.borrow().as_ref() {
                        c.st.borrow_mut().job = None;
                        match result {
                            Ok(summary) => {
                                let mut st = c.st.borrow_mut();
                                st.items.push(Item::Image(out.clone()));
                                st.after_job = Some(st.items.len() - 1);
                                drop(st);
                                c.push_sources();
                                c.progress_finish(summary, &c.t("gui-scan-image", &[]));
                            }
                            Err(e) => c.progress_fail(&e),
                        }
                    }
                });
            });
        });
    }

    fn after_job(&self) {
        let next = self.st.borrow_mut().after_job.take();
        match next {
            Some(i) => {
                self.st.borrow_mut().current = Some(i);
                self.push_sources();
                self.pick(i);
            }
            None => {
                if let Some(ui) = self.ui() {
                    ui.set_step(if self.st.borrow().session.is_some() { 2 } else { -1 });
                }
            }
        }
    }

    // ---------------------------------------------------------------- progress screens

    fn progress_start(&self, step: i32, heading: &str) {
        let Some(ui) = self.ui() else { return };
        ui.set_progress_eyebrow(sh(format!("0{} {}", step + 1, self.t(step_id(step), &[]))));
        ui.set_progress_heading(sh(heading));
        ui.set_progress_phase(sh(""));
        ui.set_progress_detail(sh(""));
        ui.set_progress_fraction(0.0);
        ui.set_progress_done(false);
        ui.set_progress_summary(model(Vec::new()));
        ui.set_progress_primary(sh(""));
        ui.set_strip(model(Vec::new()));
        ui.set_head(-1.0);
        ui.set_step(if step == 0 { 3 } else { step });
    }

    fn progress_finish(&self, summary: Vec<Count>, primary: &str) {
        let Some(ui) = self.ui() else { return };
        ui.set_progress_done(true);
        ui.set_progress_fraction(1.0);
        ui.set_progress_summary(model(summary));
        ui.set_progress_primary(sh(primary));
    }

    fn progress_fail(&self, e: &str) {
        let Some(ui) = self.ui() else { return };
        ui.set_progress_done(true);
        ui.set_progress_phase(sh(e));
        ui.set_progress_primary(sh(self.t("gui-back", &[])));
    }

    // ---------------------------------------------------------------- browse

    pub fn open_session(&self, path: &Path) {
        let Some(ui) = self.ui() else { return };
        {
            let mut st = self.st.borrow_mut();
            let known = st
                .items
                .iter()
                .position(|i| matches!(i, Item::Session(p) if p == path));
            let i = known.unwrap_or_else(|| {
                st.items.push(Item::Session(path.to_path_buf()));
                st.items.len() - 1
            });
            st.current = Some(i);
        }
        self.push_sources();
        let s = match Session::open(path) {
            Ok(s) => s,
            Err(e) => {
                self.progress_start(2, &file_name(path));
                self.progress_fail(&e.to_string());
                return;
            }
        };
        let m = match Model::load(&s) {
            Ok(m) => m,
            Err(e) => {
                self.progress_start(2, &file_name(path));
                self.progress_fail(&e.to_string());
                return;
            }
        };
        let src = s.source().ok();
        let bad: Vec<std::ops::Range<u64>> = s
            .bad_map()
            .map(|b| b.iter().map(|r| r.start * 512..r.end * 512).collect())
            .unwrap_or_default();
        let source_size = src.as_ref().map_or(0, |x| x.fingerprint.size);
        let heading = src
            .as_ref()
            .map(|x| {
                x.identity
                    .model
                    .clone()
                    .unwrap_or_else(|| file_name(Path::new(&x.identity.path)))
            })
            .unwrap_or_default();
        let (files, bytes) = m.total_files();
        let mut facts = Vec::new();
        if let Some(v) = m.volumes.iter().find(|v| v.walked) {
            let c = &v.candidate;
            facts.push(Fact {
                label: sh(""),
                value: sh(c.fs.as_str().to_uppercase()),
            });
            facts.push(Fact {
                label: sh(self.t("tui-fact-cluster", &[])),
                value: sh(size(u64::from(c.cluster_size))),
            });
            facts.push(Fact {
                label: sh(self.t("tui-fact-data", &[])),
                value: sh(format!("0x{:08X}", c.data_offset)),
            });
        }
        facts.push(Fact {
            label: sh(""),
            value: sh(self.t("gui-fact-folders", &[("count", Arg::from(m.folders()))])),
        });
        facts.push(Fact {
            label: sh(""),
            value: sh(format!(
                "{} · {}",
                self.t("gui-fact-files", &[("count", Arg::from(files))]),
                size(bytes)
            )),
        });
        let lost = m
            .entries
            .iter()
            .filter(|e| matches!(e.entry.parent, rescyo_fs::Parent::Lost { .. }))
            .count() as u64;
        let (note_title, note) = if !bad.is_empty() {
            let sectors: u64 = bad.iter().map(|r| (r.end - r.start) / 512).sum();
            (
                self.t("gui-note-bad", &[]),
                self.t("scan-bad-sectors", &[("count", Arg::from(sectors))]),
            )
        } else if lost > 0 {
            (
                self.t("gui-note-lost-title", &[]),
                self.t("tui-note-lost", &[("count", Arg::from(lost))]),
            )
        } else {
            (String::new(), String::new())
        };
        ui.set_heading(sh(heading));
        ui.set_facts(model(facts));
        ui.set_strip(model(strip_cells(source_size, &bad, None, STRIP_CELLS)));
        ui.set_head(-1.0);
        ui.set_note_title(sh(note_title));
        ui.set_note(sh(note));
        ui.set_search_placeholder(sh(self.t("gui-search", &[("count", Arg::from(files))])));
        ui.set_query(sh(""));
        {
            let mut st = self.st.borrow_mut();
            st.session = Some(s);
            st.session_path = Some(path.to_path_buf());
            st.model = Some(m);
            st.integrity.clear();
            st.source_size = source_size;
            st.bad = bad;
        }
        ui.set_tree_index(0);
        ui.set_file_index(0);
        self.refresh_tree();
        ui.set_has_session(true);
        ui.set_step(2);
    }

    fn refresh_tree(&self) {
        let Some(ui) = self.ui() else { return };
        let mut st = self.st.borrow_mut();
        let Some(m) = &st.model else { return };
        let rows = m.tree();
        let items: Vec<TreeItem> = rows
            .iter()
            .map(|r| TreeItem {
                label: sh(r.label.clone()),
                depth: i32::from(r.depth),
                expandable: r.expandable,
                expanded: r.expanded,
                lost: r.lost,
                deleted: r.deleted,
            })
            .collect();
        st.rows = rows;
        drop(st);
        ui.set_tree(model(items));
        self.refresh_files();
    }

    fn current_node(&self) -> Option<Node> {
        let ui = self.ui()?;
        let st = self.st.borrow();
        st.rows
            .get(usize::try_from(ui.get_tree_index()).ok()?)
            .map(|r| r.node)
    }

    fn toggle(&self, i: usize) {
        let node = self.st.borrow().rows.get(i).map(|r| r.node);
        if let (Some(n), Some(m)) = (node, self.st.borrow_mut().model.as_mut()) {
            m.toggle(n);
        }
        self.refresh_tree();
    }

    fn refresh_files(&self) {
        let Some(ui) = self.ui() else { return };
        let node = self.current_node();
        let query = ui.get_query().to_string();
        let st = self.st.borrow();
        let Some(m) = &st.model else { return };
        let list = match node {
            Some(n) => m.files(n, &query),
            None => Vec::new(),
        };
        let ids: Vec<u64> = list.iter().map(|e| e.id.0).collect();
        let rows: Vec<FileRow> = list
            .iter()
            .map(|e| {
                let x = &e.entry;
                let (chip, kind) = assembly_chip(x.assembly);
                FileRow {
                    name: sh(x.name.clone()),
                    size: sh(size(x.size)),
                    modified: sh(x
                        .times
                        .modified
                        .map(|t| short_time(&t.to_rfc3339()))
                        .unwrap_or_default()),
                    chip: sh(chip),
                    chip_kind: i32::from(kind),
                    integrity: integrity_state(st.integrity.get(&e.id.0)),
                    marked: m.marked.contains(&e.id.0),
                    deleted: x.state == EntryState::Deleted,
                }
            })
            .collect();
        drop(st);
        self.st.borrow_mut().file_ids = ids;
        let idx = ui
            .get_file_index()
            .clamp(0, i32::try_from(rows.len()).unwrap_or(0).saturating_sub(1).max(0));
        ui.set_files(model(rows));
        ui.set_file_index(idx);
        self.refresh_inspector();
        self.refresh_preflight();
    }

    fn mark(&self, i: usize) {
        let id = self.st.borrow().file_ids.get(i).copied();
        if let (Some(id), Some(m)) = (id, self.st.borrow_mut().model.as_mut()) {
            m.toggle_mark(id);
        }
        self.refresh_files();
    }

    fn refresh_inspector(&self) {
        let Some(ui) = self.ui() else { return };
        let st = self.st.borrow();
        let id = usize::try_from(ui.get_file_index())
            .ok()
            .and_then(|i| st.file_ids.get(i).copied());
        let Some(e) = id.and_then(|id| st.model.as_ref()?.entry(id)) else {
            ui.set_inspector_name(sh(""));
            ui.set_inspector(model(Vec::new()));
            ui.set_has_preview(false);
            return;
        };
        let x = &e.entry;
        let (chip, _) = assembly_chip(x.assembly);
        let runs = x
            .layout
            .extents
            .iter()
            .filter(|e| matches!(e, Extent::Disk { .. }))
            .count();
        let integ = integrity_state(st.integrity.get(&e.id.0));
        let word = ["tui-intact", "tui-partial", "tui-damaged", "tui-unverified"]
            .get(usize::try_from(integ).unwrap_or(3))
            .copied()
            .unwrap_or("tui-unverified");
        let path = st
            .session
            .as_ref()
            .and_then(|s| s.path_of(e.id).ok())
            .unwrap_or_default();
        let kv = |k: &str, v: String, mono: bool| Kv {
            key: sh(self.t(k, &[])),
            value: sh(v),
            mono,
        };
        let mut rows = vec![
            kv("tui-col-assembly", chip.to_lowercase(), false),
            kv("tui-col-integrity", self.t(word, &[]), false),
            kv("tui-col-size", format!("{} ({})", size(x.size), x.size), true),
            kv("tui-insp-runs", runs.to_string(), true),
            kv(
                "tui-insp-state",
                self.t(
                    if x.state == EntryState::Deleted {
                        "tui-deleted"
                    } else {
                        "tui-live"
                    },
                    &[],
                ),
                false,
            ),
        ];
        if x.kind == EntryKind::Stream {
            rows.push(kv("tui-insp-kind", self.t("tui-stream", &[]), false));
        }
        rows.push(kv("gui-insp-path", path, true));
        ui.set_inspector_name(sh(x.name.clone()));
        ui.set_inspector(model(rows));
        ui.set_has_preview(false);
    }

    fn recover_ids(&self) -> Vec<u64> {
        let st = self.st.borrow();
        let Some(m) = &st.model else { return Vec::new() };
        if !m.marked.is_empty() {
            return m.marked.iter().copied().collect();
        }
        drop(st);
        let node = self.current_node();
        let st = self.st.borrow();
        match (node, &st.model) {
            (Some(n), Some(m)) => m.files_below(n),
            _ => Vec::new(),
        }
    }

    fn options(&self) -> Option<RecoverOptions> {
        Some(RecoverOptions {
            target: self.st.borrow().target.clone()?,
            selection: Selection {
                deleted: DeletedMode::Include,
                streams: true,
                ids: self.recover_ids(),
                ..Selection::default()
            },
            allow_unverified_target: false,
            verify: true,
        })
    }

    fn refresh_preflight(&self) {
        let Some(ui) = self.ui() else { return };
        let count = self.recover_ids().len();
        ui.set_recover_count(i32::try_from(count).unwrap_or(i32::MAX));
        crate::strings::set_recover_button(&ui, &self.l, count as u64);
        let target = self.st.borrow().target.clone();
        ui.set_target(sh(target
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()));
        let Some(opts) = self.options() else {
            ui.set_preflight_ok(false);
            ui.set_preflight(sh(""));
            return;
        };
        let st = self.st.borrow();
        let Some(s) = &st.session else { return };
        match rescyo_core::plan(s, &SystemResolver, &opts) {
            Ok(p) if p.problems.is_empty() => {
                ui.set_preflight_ok(true);
                ui.set_preflight(sh(self.t(
                    "tui-preflight-ok",
                    &[("free", Arg::from(size(p.target_info.free_bytes.unwrap_or(0))))],
                )));
            }
            Ok(p) => {
                ui.set_preflight_ok(false);
                ui.set_preflight(sh(p
                    .problems
                    .first()
                    .map(|x| problem_text(&self.l, x))
                    .unwrap_or_default()));
            }
            Err(e) => {
                ui.set_preflight_ok(false);
                ui.set_preflight(sh(e.to_string()));
            }
        }
    }

    // ---------------------------------------------------------------- recovery

    fn recover(self: &Rc<Self>) {
        let Some(opts) = self.options() else { return };
        let Some(session) = self.st.borrow().session_path.clone() else {
            return;
        };
        let cancel = CancelToken::new();
        self.st.borrow_mut().job = Some(cancel.clone());
        let n = opts.selection.ids.len() as u64;
        self.progress_start(3, &self.t("gui-recovering", &[("count", Arg::from(n))]));
        let weak = self.ui.clone();
        std::thread::spawn(move || {
            let result = recover_job(&session, &opts, &cancel, &weak);
            let _ = slint::invoke_from_event_loop(move || {
                CONTROLLER.with(|c| {
                    if let Some(c) = c.borrow().as_ref() {
                        let l = c.l.clone();
                        c.st.borrow_mut().job = None;
                        match result {
                            Ok((marks, totals)) => {
                                c.st.borrow_mut().integrity.extend(marks);
                                if let Some(m) = c.st.borrow_mut().model.as_mut() {
                                    m.marked.clear();
                                }
                                let rows = vec![
                                    Count {
                                        state: 0,
                                        label: sh(l.t("tui-intact", &[])),
                                        value: sh(totals.intact.to_string()),
                                    },
                                    Count {
                                        state: 1,
                                        label: sh(l.t("tui-partial", &[])),
                                        value: sh(totals.partial.to_string()),
                                    },
                                    Count {
                                        state: 2,
                                        label: sh(l.t("tui-damaged", &[])),
                                        value: sh(totals.damaged.to_string()),
                                    },
                                    Count {
                                        state: 3,
                                        label: sh(l.t("tui-unverified", &[])),
                                        value: sh(totals.unverified.to_string()),
                                    },
                                    Count {
                                        state: -1,
                                        label: sh(l.t("gui-sum-failed", &[])),
                                        value: sh(totals.failed.to_string()),
                                    },
                                ];
                                if let Some(ui) = c.ui() {
                                    ui.set_progress_phase(sh(l.t(
                                        "gui-recovered",
                                        &[
                                            ("files", Arg::from(totals.files - totals.failed)),
                                            ("size", Arg::from(size(totals.bytes))),
                                        ],
                                    )));
                                }
                                c.progress_finish(rows, &l.t("gui-back", &[]));
                                c.refresh_files();
                            }
                            Err(e) => c.progress_fail(&e),
                        }
                    }
                });
            });
        });
    }
}

thread_local! {
    /// The controller of the window, for callbacks posted back from worker threads.
    pub static CONTROLLER: RefCell<Option<Rc<Controller>>> = const { RefCell::new(None) };
}

fn step_id(step: i32) -> &'static str {
    match step {
        0 => "tui-step-source",
        1 => "tui-step-scan",
        2 => "tui-step-browse",
        _ => "tui-step-recover",
    }
}

fn integrity_state(i: Option<&Integrity>) -> i32 {
    match i {
        Some(Integrity::Intact) => 0,
        Some(Integrity::Partial) => 1,
        Some(Integrity::Damaged) => 2,
        _ => 3,
    }
}

fn short_time(s: &str) -> String {
    s.get(..16)
        .map(|t| t.replace('T', " "))
        .unwrap_or_else(|| s.to_owned())
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

fn problem_text(l: &Localizer, p: &Problem) -> String {
    let s = |v: &str| Arg::from(v.to_owned());
    match p {
        Problem::TargetOnSourceDevice { devices } => l.t(
            "problem-target-on-source-device",
            &[("devices", s(&devices.join(", ")))],
        ),
        Problem::UnverifiedTarget { reason } => l.t("problem-unverified-target", &[("reason", s(reason))]),
        Problem::SourceDevicesUnknown { reason } => {
            l.t("problem-source-devices-unknown", &[("reason", s(reason))])
        }
        Problem::InsufficientSpace { needed, free } => l.t(
            "problem-insufficient-space",
            &[
                ("needed", Arg::from(size(*needed))),
                ("free", Arg::from(size(*free))),
            ],
        ),
        Problem::FileTooLarge {
            path,
            size: sz,
            limit,
        } => l.t(
            "problem-file-too-large",
            &[
                ("path", s(path)),
                ("size", Arg::from(size(*sz))),
                ("limit", Arg::from(size(*limit))),
            ],
        ),
        Problem::NothingSelected => l.t("problem-nothing-selected", &[]),
    }
}

/// Posts progress to the window from a worker thread.
fn post(weak: &Weak<MainWindow>, f: impl FnOnce(&MainWindow) + Send + 'static) {
    let _ = weak.upgrade_in_event_loop(move |ui| f(&ui));
}

fn phase_text(l: &Localizer, p: Phase) -> String {
    l.t(
        match p {
            Phase::Probe => "phase-probe",
            Phase::DeepScan => "phase-deep-scan",
            Phase::Walk => "phase-walk",
            Phase::LostFolders => "phase-lost-folders",
        },
        &[],
    )
}

fn scan_job(
    path: &str,
    session: &Path,
    deep: bool,
    carve: bool,
    cancel: &CancelToken,
    weak: &Weak<MainWindow>,
    l: &Localizer,
) -> Result<(), String> {
    let r = open_source(path)?;
    if let Some(dir) = session.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        rescyo_core::check_destination(&SystemResolver, r.identity(), dir, false)
            .map_err(|p| problem_text(l, &p))?;
    }
    let fp = SourceFingerprint::compute(&r).map_err(|e| e.to_string())?;
    let mut s = Session::create(session, r.identity(), &fp).map_err(|e| e.to_string())?;
    let total = r.geometry().size;
    let opts = ScanOptions {
        mode: if deep { ScanMode::Deep } else { ScanMode::Quick },
        carve,
        ..ScanOptions::default()
    };
    let mut last = std::time::Instant::now();
    rescyo_core::scan(
        &r,
        &mut s,
        &opts,
        &mut |p| {
            if last.elapsed() < std::time::Duration::from_millis(120) {
                return;
            }
            last = std::time::Instant::now();
            let phase = phase_text(l, p.phase);
            let head = (p.phase == Phase::DeepScan).then_some(p.done);
            let bad: Vec<std::ops::Range<u64>> =
                r.bad_map().iter().map(|x| x.start * 512..x.end * 512).collect();
            let cells = strip_cells(total, &bad, head.or(Some(0)), STRIP_CELLS);
            let frac = p
                .total
                .filter(|&t| t > 0)
                .map_or(0.0, |t| p.done as f32 / t as f32);
            let detail = match p.total {
                Some(t) if p.phase == Phase::DeepScan => {
                    format!("{:.1}%  {} / {}", frac * 100.0, size(p.done), size(t))
                }
                Some(t) => format!("{} / {t}", p.done),
                None => p.done.to_string(),
            };
            post(weak, move |ui| {
                ui.set_progress_phase(sh(phase));
                ui.set_progress_detail(sh(detail));
                ui.set_progress_fraction(frac);
                ui.set_strip(model(cells));
                ui.set_head(head.map_or(-1.0, |h| h as f32 / total.max(1) as f32));
            });
        },
        cancel,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn image_job(
    path: &str,
    out: &Path,
    cancel: &CancelToken,
    weak: &Weak<MainWindow>,
    l: &Localizer,
) -> Result<Vec<Count>, String> {
    let src = open_source(path)?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        rescyo_core::check_destination(&SystemResolver, src.identity(), dir, false)
            .map_err(|p| problem_text(l, &p))?;
    }
    let map = {
        let mut m = out.as_os_str().to_owned();
        m.push(".map");
        PathBuf::from(m)
    };
    let mut last = std::time::Instant::now();
    let report = rescyo_core::image(
        src.source().as_ref(),
        out,
        &map,
        &rescyo_core::ImageOptions::default(),
        &mut |p| {
            if last.elapsed() < std::time::Duration::from_millis(120) {
                return;
            }
            last = std::time::Instant::now();
            let frac = p.pos as f32 / p.size.max(1) as f32;
            let detail = format!("{}  ·  {} bad", size(p.rescued), size(p.bad));
            post(weak, move |ui| {
                ui.set_progress_fraction(frac);
                ui.set_progress_detail(sh(detail));
            });
        },
        cancel,
    )
    .map_err(|e| e.to_string())?;
    Ok(vec![
        Count {
            state: 0,
            label: sh(l.t("image-rescued", &[])),
            value: sh(size(report.rescued)),
        },
        Count {
            state: 2,
            label: sh(l.t("image-bad-short", &[])),
            value: sh(size(report.bad_bytes)),
        },
    ])
}

type Marks = HashMap<u64, Integrity>;

fn recover_job(
    session: &Path,
    opts: &RecoverOptions,
    cancel: &CancelToken,
    weak: &Weak<MainWindow>,
) -> Result<(Marks, rescyo_core::recovery::report::Totals), String> {
    let s = Session::open(session).map_err(|e| e.to_string())?;
    let rec = s.source().map_err(|e| e.to_string())?;
    let r = open_source(&rec.identity.path)?;
    let fresh = SourceFingerprint::compute(&r).map_err(|e| e.to_string())?;
    if !matches!(
        rec.fingerprint.compare(&fresh),
        rescyo_io::FingerprintMatch::Match
    ) {
        return Err("the source no longer matches this session".into());
    }
    let plan = rescyo_core::plan(&s, &SystemResolver, opts).map_err(|e| e.to_string())?;
    let mut last = std::time::Instant::now();
    let report = rescyo_core::run(
        &r,
        &s,
        &plan,
        &mut |p| {
            if last.elapsed() < std::time::Duration::from_millis(120) {
                return;
            }
            last = std::time::Instant::now();
            let frac = p.bytes_done as f32 / p.bytes_total.max(1) as f32;
            let detail = format!(
                "{} / {}  ·  {} / {}",
                size(p.bytes_done),
                size(p.bytes_total),
                p.files_done,
                p.files_total
            );
            post(weak, move |ui| {
                ui.set_progress_fraction(frac);
                ui.set_progress_detail(sh(detail));
            });
        },
        cancel,
    )
    .map_err(|e| e.to_string())?;
    let by_path: HashMap<&str, u64> = plan
        .items
        .iter()
        .map(|i| (i.source_path.as_str(), i.entry.id.0))
        .collect();
    let marks = report
        .files
        .iter()
        .filter_map(|f| Some((*by_path.get(f.source_path.as_str())?, f.integrity)))
        .collect();
    Ok((marks, report.totals))
}
