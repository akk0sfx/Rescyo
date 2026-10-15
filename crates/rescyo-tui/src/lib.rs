//! Rescyo's terminal interface: the same flow as the GUI (source, scan, browse, recover) with
//! the same design tokens, for consoles, SSH sessions and live-USB systems.
#![forbid(unsafe_code)]

mod app;
mod theme;
mod ui;

use std::io;
use std::path::Path;

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use rescyo_core::{ScanOptions, Session};
use rescyo_fs::CancelToken;
use rescyo_i18n::{Arg, Localizer};
use rescyo_io::GuardedReader;

pub use app::Opener;
use app::{Action, App, Mode, Step, Throttle, esc_pressed, phase_id};
pub use rescyo_core::browse::Model;
pub use theme::{Depth, Theme};

/// How the interface looks.
#[derive(Debug, Clone)]
pub struct Options {
    pub lang: Option<String>,
    pub dark: bool,
}

/// Restores the terminal however the interface ends, panics included.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            previous(info);
        }));
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

fn new_app(session: &Session, opts: &Options, title: String) -> io::Result<App> {
    let model = Model::load(session).map_err(io::Error::other)?;
    let theme = Theme::new(opts.dark, Depth::detect());
    let mut app = App::new(theme, Localizer::new(opts.lang.as_deref()), model, title);
    app.load_session_facts(session);
    Ok(app)
}

/// Browses an existing Session and recovers from it.
pub fn browse(session: &Path, opener: &Opener<'_>, opts: &Options) -> io::Result<()> {
    let s = Session::open(session).map_err(io::Error::other)?;
    let mut app = new_app(&s, opts, String::new())?;
    let _guard = TerminalGuard::enter()?;
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    event_loop(&mut term, &mut app, &s, opener)
}

/// Scans `r` into the new Session `s` with a live progress screen, then browses it.
pub fn scan_and_browse(
    r: &GuardedReader,
    mut s: Session,
    title: &str,
    scan: &ScanOptions,
    opener: &Opener<'_>,
    opts: &Options,
) -> io::Result<()> {
    let _guard = TerminalGuard::enter()?;
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    // The scan screen needs an App before the Session has entries.
    let theme = Theme::new(opts.dark, Depth::detect());
    let empty = Model::from_parts(Vec::new(), Vec::new());
    let mut app = App::new(
        theme,
        Localizer::new(opts.lang.as_deref()),
        empty,
        title.to_owned(),
    );
    app.step = Step::Scan;
    app.source_size = r.geometry().size;
    app.read_until = Some(0);
    let cancel = CancelToken::new();
    let mut throttle = Throttle::new();
    let result = rescyo_core::scan(
        r,
        &mut s,
        scan,
        &mut |p| {
            app.scan.phase = phase_id(p.phase).to_owned();
            app.scan.done = p.done;
            app.scan.total = p.total;
            if p.phase == rescyo_fs::Phase::DeepScan {
                app.read_until = Some(p.done);
            }
            if throttle.due() {
                app.bad = r.bad_map().iter().map(|x| x.start * 512..x.end * 512).collect();
                let _ = term.draw(|f| ui::draw(f, &app));
                if esc_pressed() {
                    cancel.cancel();
                }
            }
        },
        &cancel,
    );
    if let Err(e) = result {
        return Err(io::Error::other(e.to_string()));
    }
    let mut app = new_app(&s, opts, title.to_owned())?;
    event_loop(&mut term, &mut app, &s, opener)
}

fn event_loop<B: Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
    s: &Session,
    opener: &Opener<'_>,
) -> io::Result<()>
where
    B::Error: Send + Sync + 'static,
{
    loop {
        app.current_path = app.current_file().and_then(|e| s.path_of(e.id).ok());
        term.draw(|f| ui::draw(f, app)).map_err(io::Error::other)?;
        if app.quit {
            return Ok(());
        }
        if !event::poll(std::time::Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(k) = event::read()? else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        match app.key(k) {
            Action::None => {}
            Action::Preflight => app.refresh_preflight(s),
            Action::Recover => recover(term, app, s, opener)?,
        }
    }
}

fn recover<B: Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
    s: &Session,
    opener: &Opener<'_>,
) -> io::Result<()>
where
    B::Error: Send + Sync + 'static,
{
    app.refresh_preflight(s);
    if !matches!(app.preflight, app::Preflight::Ok { .. }) {
        if app.target.is_none() {
            app.mode = Mode::Input(String::new());
        }
        return Ok(());
    }
    let Some(opts) = app.target.clone().map(|target| rescyo_core::RecoverOptions {
        target,
        selection: rescyo_core::Selection {
            deleted: rescyo_core::DeletedMode::Include,
            streams: true,
            ids: app.recover_ids(),
            ..rescyo_core::Selection::default()
        },
        allow_unverified_target: false,
        verify: true,
    }) else {
        return Ok(());
    };
    let plan = rescyo_core::plan(s, &rescyo_core::SystemResolver, &opts).map_err(io::Error::other)?;
    // The Source must still be the one the Session was made from.
    let src = s.source().map_err(io::Error::other)?;
    let r = match opener(&src.identity.path) {
        Ok(r) => r,
        Err(e) => {
            app.mode = Mode::Message(vec![e]);
            return Ok(());
        }
    };
    let fresh = rescyo_io::SourceFingerprint::compute(&r).map_err(io::Error::other)?;
    if !matches!(
        src.fingerprint.compare(&fresh),
        rescyo_io::FingerprintMatch::Match
    ) {
        app.mode = Mode::Message(vec![app.t("tui-source-changed", &[])]);
        return Ok(());
    }
    app.step = app::Step::Recover;
    let cancel = CancelToken::new();
    let mut throttle = Throttle::new();
    let result = rescyo_core::run(
        &r,
        s,
        &plan,
        &mut |p| {
            app.mode = Mode::Running {
                done: p.bytes_done,
                total: p.bytes_total,
            };
            if throttle.due() {
                let _ = term.draw(|f| ui::draw(f, app));
                if esc_pressed() {
                    cancel.cancel();
                }
            }
        },
        &cancel,
    );
    app.step = app::Step::Browse;
    app.mode = match result {
        Ok(report) => {
            // Integrity marks for what was just recovered.
            let by_path: std::collections::HashMap<&str, u64> = plan
                .items
                .iter()
                .map(|i| (i.source_path.as_str(), i.entry.id.0))
                .collect();
            for f in &report.files {
                if let Some(&id) = by_path.get(f.source_path.as_str()) {
                    app.integrity.insert(id, f.integrity);
                }
            }
            let t = &report.totals;
            Mode::Message(vec![
                app.t(
                    "recover-done",
                    &[
                        ("files", Arg::from(t.files - t.failed)),
                        ("size", Arg::from(rescyo_i18n::size(t.bytes))),
                        (
                            "report",
                            Arg::from(
                                opts.target
                                    .join(rescyo_core::recovery::report::JSON_NAME)
                                    .display()
                                    .to_string(),
                            ),
                        ),
                    ],
                ),
                app.t("recover-intact", &[("count", Arg::from(t.intact))]),
            ])
        }
        Err(rescyo_core::RecoveryError::Cancelled) => Mode::Message(vec![app.t("tui-cancelled", &[])]),
        Err(e) => Mode::Message(vec![e.to_string()]),
    };
    app.model.marked.clear();
    Ok(())
}

#[cfg(test)]
mod tests;
