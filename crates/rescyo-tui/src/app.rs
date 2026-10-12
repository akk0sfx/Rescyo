//! State, keys and the two long operations (scan, recovery), run on the UI thread with the
//! screen redrawn from their progress callbacks; Esc cancels.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use rescyo_core::recovery::report::Integrity;
use rescyo_core::{Problem, RecoverOptions, Selection, Session, StoredEntry, SystemResolver};
use rescyo_fs::Phase;
use rescyo_i18n::{Arg, Localizer, size};
use rescyo_io::GuardedReader;

use crate::theme::Theme;
use crate::ui::Mark;
use rescyo_core::browse::{Model, Node, TreeRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Source,
    Scan,
    Browse,
    Recover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Files,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    Input(String),
    Running { done: u64, total: u64 },
    Message(Vec<String>),
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preflight {
    Unknown,
    Ok { free: u64 },
    Problems(Vec<String>),
}

#[derive(Debug, Clone, Default)]
pub struct ScanState {
    pub phase: String,
    pub done: u64,
    pub total: Option<u64>,
}

/// Opens the Source a Session was made from (the CLI's own opener, with its policies).
pub type Opener<'a> = dyn Fn(&str) -> Result<GuardedReader, String> + 'a;

pub struct App {
    pub theme: Theme,
    pub l10n: Localizer,
    pub model: Model,
    pub title: String,
    pub facts: Vec<(String, String)>,
    pub notes: Vec<String>,
    pub source_size: u64,
    pub bad: Vec<Range<u64>>,
    /// During a scan: how far the read has come (the strip's read head).
    pub read_until: Option<u64>,
    pub step: Step,
    pub focus: Focus,
    pub tree_sel: usize,
    pub file_sel: usize,
    pub query: String,
    pub mode: Mode,
    pub target: Option<PathBuf>,
    pub preflight: Preflight,
    pub scan: ScanState,
    pub integrity: HashMap<u64, Integrity>,
    pub current_path: Option<String>,
    pub quit: bool,
}

impl App {
    pub fn new(theme: Theme, l10n: Localizer, model: Model, title: String) -> Self {
        Self {
            theme,
            l10n,
            model,
            title,
            facts: Vec::new(),
            notes: Vec::new(),
            source_size: 0,
            bad: Vec::new(),
            read_until: None,
            step: Step::Browse,
            focus: Focus::Files,
            tree_sel: 0,
            file_sel: 0,
            query: String::new(),
            mode: Mode::Normal,
            target: None,
            preflight: Preflight::Unknown,
            scan: ScanState::default(),
            integrity: HashMap::new(),
            current_path: None,
            quit: false,
        }
    }

    pub fn t(&self, id: &str, args: &[(&str, Arg<'_>)]) -> String {
        self.l10n.t(id, args)
    }

    /// Facts, strip and notes from the Session.
    pub fn load_session_facts(&mut self, s: &Session) {
        if let Ok(src) = s.source() {
            self.source_size = src.fingerprint.size;
            if self.title.is_empty() {
                self.title = src.identity.model.clone().unwrap_or(src.identity.path.clone());
            }
        }
        if let Ok(bad) = s.bad_map() {
            self.bad = bad.iter().map(|r| r.start * 512..r.end * 512).collect();
        }
        if let Some(v) = self.model.volumes.iter().find(|v| v.walked) {
            let c = &v.candidate;
            self.facts.push((String::new(), c.fs.as_str().to_uppercase()));
            self.facts
                .push((self.t("tui-fact-cluster", &[]), size(u64::from(c.cluster_size))));
            self.facts
                .push((self.t("tui-fact-data", &[]), format!("0x{:08X}", c.data_offset)));
        }
        let (files, bytes) = self.model.total_files();
        self.facts
            .push((self.t("tui-fact-folders", &[]), self.model.folders().to_string()));
        self.facts.push((
            self.t("tui-fact-files", &[]),
            format!("{files} · {}", size(bytes)),
        ));
        if !self.bad.is_empty() {
            let sectors: u64 = self.bad.iter().map(|r| (r.end - r.start) / 512).sum();
            self.notes
                .push(self.t("scan-bad-sectors", &[("count", Arg::from(sectors))]));
        }
        if let Ok(j) = s.journal_entries() {
            let lost: u64 = self
                .model
                .entries
                .iter()
                .filter(|e| matches!(e.entry.parent, rescyo_fs::Parent::Lost { .. }))
                .count() as u64;
            if lost > 0 {
                self.notes
                    .push(self.t("tui-note-lost", &[("count", Arg::from(lost))]));
            }
            let _ = j;
        }
    }

    pub fn tree_rows(&self) -> Vec<TreeRow> {
        self.model.tree()
    }

    pub fn current_node(&self) -> Option<Node> {
        self.tree_rows().get(self.tree_sel).map(|r| r.node)
    }

    pub fn current_files(&self) -> Vec<&StoredEntry> {
        match self.current_node() {
            Some(n) => self.model.files(n, &self.query),
            None if !self.query.is_empty() => self.model.files(Node::Volume(0), &self.query),
            None => Vec::new(),
        }
    }

    pub fn current_file(&self) -> Option<&StoredEntry> {
        self.current_files().get(self.file_sel).copied()
    }

    pub fn integrity_of(&self, id: u64) -> Mark {
        match self.integrity.get(&id) {
            Some(Integrity::Intact) => Mark::Intact,
            Some(Integrity::Partial) => Mark::Partial,
            Some(Integrity::Damaged) => Mark::Damaged,
            _ => Mark::Unverified,
        }
    }

    /// The files a Recovery would copy: the marked ones, else everything below the folder.
    pub fn recover_ids(&self) -> Vec<u64> {
        if !self.model.marked.is_empty() {
            return self.model.marked.iter().copied().collect();
        }
        self.current_node()
            .map(|n| self.model.files_below(n))
            .unwrap_or_default()
    }

    pub fn recover_count(&self) -> u64 {
        self.recover_ids().len() as u64
    }

    fn problem_text(&self, p: &Problem) -> String {
        let s = |v: &str| Arg::from(v.to_owned());
        match p {
            Problem::TargetOnSourceDevice { devices } => self.t(
                "problem-target-on-source-device",
                &[("devices", s(&devices.join(", ")))],
            ),
            Problem::UnverifiedTarget { reason } => {
                self.t("problem-unverified-target", &[("reason", s(reason))])
            }
            Problem::SourceDevicesUnknown { reason } => {
                self.t("problem-source-devices-unknown", &[("reason", s(reason))])
            }
            Problem::InsufficientSpace { needed, free } => self.t(
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
            } => self.t(
                "problem-file-too-large",
                &[
                    ("path", s(path)),
                    ("size", Arg::from(size(*sz))),
                    ("limit", Arg::from(size(*limit))),
                ],
            ),
            Problem::NothingSelected => self.t("problem-nothing-selected", &[]),
        }
    }

    fn options(&self) -> Option<RecoverOptions> {
        Some(RecoverOptions {
            target: self.target.clone()?,
            selection: Selection {
                deleted: rescyo_core::DeletedMode::Include,
                streams: true,
                ids: self.recover_ids(),
                ..Selection::default()
            },
            allow_unverified_target: false,
            verify: true,
        })
    }

    /// Re-runs the Preflight for the current selection and target.
    pub fn refresh_preflight(&mut self, s: &Session) {
        let Some(opts) = self.options() else {
            self.preflight = Preflight::Unknown;
            return;
        };
        self.preflight = match rescyo_core::plan(s, &SystemResolver, &opts) {
            Ok(p) if p.problems.is_empty() => Preflight::Ok {
                free: p.target_info.free_bytes.unwrap_or(0),
            },
            Ok(p) => Preflight::Problems(p.problems.iter().map(|x| self.problem_text(x)).collect()),
            Err(e) => Preflight::Problems(vec![e.to_string()]),
        };
    }

    fn move_sel(&mut self, delta: isize) {
        let (sel, len) = match self.focus {
            Focus::Tree => (&mut self.tree_sel, self.model.tree().len()),
            Focus::Files => {
                let len = self.current_files().len();
                (&mut self.file_sel, len)
            }
        };
        let max = len.saturating_sub(1);
        *sel = sel.saturating_add_signed(delta).min(max);
        if self.focus == Focus::Tree {
            self.file_sel = 0;
        }
    }

    /// Handles one key; returns true when the selection or target changed (Preflight is due).
    pub fn key(&mut self, k: KeyEvent) -> Action {
        match &mut self.mode {
            Mode::Search => {
                match k.code {
                    KeyCode::Esc => {
                        self.query.clear();
                        self.mode = Mode::Normal;
                    }
                    KeyCode::Enter => self.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        self.query.pop();
                    }
                    KeyCode::Char(c) => self.query.push(c),
                    _ => {}
                }
                self.file_sel = 0;
                return Action::None;
            }
            Mode::Input(text) => {
                match k.code {
                    KeyCode::Esc => self.mode = Mode::Normal,
                    KeyCode::Enter => {
                        let t = text.trim().to_owned();
                        self.mode = Mode::Normal;
                        if !t.is_empty() {
                            self.target = Some(PathBuf::from(t));
                            return Action::Preflight;
                        }
                    }
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    KeyCode::Char(c) => text.push(c),
                    _ => {}
                }
                return Action::None;
            }
            Mode::Message(_) | Mode::Help => {
                self.mode = Mode::Normal;
                return Action::None;
            }
            Mode::Running { .. } => return Action::None,
            Mode::Normal => {}
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('?') | KeyCode::F(1) => self.mode = Mode::Help,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::Tree {
                    Focus::Files
                } else {
                    Focus::Tree
                };
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-20),
            KeyCode::PageDown => self.move_sel(20),
            KeyCode::Home => self.move_sel(isize::MIN / 2),
            KeyCode::End => self.move_sel(isize::MAX / 2),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter if self.focus == Focus::Tree => {
                if let Some(n) = self.current_node()
                    && !self.model.expanded.contains(&n)
                {
                    self.model.toggle(n);
                }
                return Action::Preflight;
            }
            KeyCode::Left | KeyCode::Char('h') if self.focus == Focus::Tree => {
                if let Some(n) = self.current_node()
                    && self.model.expanded.contains(&n)
                {
                    self.model.toggle(n);
                }
            }
            KeyCode::Char(' ') if self.focus == Focus::Files => {
                if let Some(id) = self.current_file().map(|e| e.id.0) {
                    self.model.toggle_mark(id);
                    self.move_sel(1);
                    return Action::Preflight;
                }
            }
            KeyCode::Char('a') => {
                let ids: Vec<u64> = self.current_files().iter().map(|e| e.id.0).collect();
                let all = ids.iter().all(|i| self.model.marked.contains(i));
                for i in ids {
                    if all {
                        self.model.marked.remove(&i);
                    } else {
                        self.model.marked.insert(i);
                    }
                }
                return Action::Preflight;
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Search;
                self.focus = Focus::Files;
            }
            KeyCode::Char('d') => {
                self.model.show_deleted = !self.model.show_deleted;
                self.tree_sel = 0;
                self.file_sel = 0;
            }
            KeyCode::Char('s') => self.model.show_streams = !self.model.show_streams,
            KeyCode::Char('t') => {
                let current = self
                    .target
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.mode = Mode::Input(current);
            }
            KeyCode::Char('r') => return Action::Recover,
            _ => {}
        }
        Action::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Preflight,
    Recover,
}

/// Whether Esc was pressed since the last call (non-blocking), for long operations.
pub fn esc_pressed() -> bool {
    while event::poll(Duration::ZERO).unwrap_or(false) {
        if let Ok(Event::Key(k)) = event::read()
            && k.kind == KeyEventKind::Press
            && k.code == KeyCode::Esc
        {
            return true;
        }
    }
    false
}

/// Throttles redraws from progress callbacks.
pub struct Throttle(Option<Instant>);

impl Throttle {
    pub fn new() -> Self {
        Self(None)
    }

    pub fn due(&mut self) -> bool {
        let now = Instant::now();
        if self
            .0
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(100))
        {
            return false;
        }
        self.0 = Some(now);
        true
    }
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new()
    }
}

pub fn phase_id(p: Phase) -> &'static str {
    match p {
        Phase::Probe => "phase-probe",
        Phase::DeepScan => "phase-deep-scan",
        Phase::Walk => "phase-walk",
        Phase::LostFolders => "phase-lost-folders",
    }
}
