//! Drawing, following the design system (docs/design): the four numbered flow steps, the
//! session header with facts, sector strip and notes, tree · files · inspector, and the safety
//! bar with the one primary action. Colours come only from tokens.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap};
use rescyo_fs::{AssemblyMethod, EntryKind, EntryState, Extent};
use rescyo_i18n::{Arg, size};

use crate::app::{App, Focus, Mode, Preflight, Step};

/// How a file's bytes were found, as a design-system chip: (label, kind).
pub fn chip(a: AssemblyMethod) -> (&'static str, ChipKind) {
    match a {
        AssemblyMethod::FatChain => ("FAT CHAIN", ChipKind::Neutral),
        AssemblyMethod::FatChainMirror => ("MIRROR FAT", ChipKind::Mirror),
        AssemblyMethod::ExfatBitmapContiguous => ("EXFAT", ChipKind::Neutral),
        AssemblyMethod::MftRunlist => ("MFT", ChipKind::Neutral),
        AssemblyMethod::Ext4Extents => ("EXTENTS", ChipKind::Neutral),
        AssemblyMethod::BlockMap => ("BLOCK MAP", ChipKind::Neutral),
        AssemblyMethod::HfsExtents => ("EXTENTS", ChipKind::Neutral),
        AssemblyMethod::ContiguousAssumed => ("CONTIGUOUS?", ChipKind::Guess),
        AssemblyMethod::Carved => ("CARVED", ChipKind::Carved),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipKind {
    Neutral,
    Mirror,
    Guess,
    Carved,
}

/// Integrity of a file as glyph, word id and colour role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Intact,
    Partial,
    Damaged,
    Unverified,
}

impl Mark {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Intact => "●",
            Self::Partial => "◐",
            Self::Damaged => "✕",
            Self::Unverified => "○",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Intact => "tui-intact",
            Self::Partial => "tui-partial",
            Self::Damaged => "tui-damaged",
            Self::Unverified => "tui-unverified",
        }
    }
}

pub fn draw(f: &mut Frame<'_>, app: &App) {
    let th = &app.theme;
    let area = f.area();
    f.render_widget(Block::default().style(th.ground(th.p.ink_1)), area);
    let [top, head, body, safety, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Min(5),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .areas(area);
    top_bar(f, app, top);
    header(f, app, head);
    match app.step {
        Step::Scan => scan_screen(f, app, body),
        _ => browse(f, app, body),
    }
    safety_bar(f, app, safety);
    let hint = Paragraph::new(app.t("tui-keys", &[])).style(th.on(th.p.text_3, th.p.ink_0));
    f.render_widget(hint, keys);
    overlay(f, app, area);
}

fn top_bar(f: &mut Frame<'_>, app: &App, area: Rect) {
    let th = &app.theme;
    let steps = [
        (Step::Source, "tui-step-source"),
        (Step::Scan, "tui-step-scan"),
        (Step::Browse, "tui-step-browse"),
        (Step::Recover, "tui-step-recover"),
    ];
    let mut spans = vec![Span::styled(
        " Rescyo  ",
        th.fg(th.p.rescue).add_modifier(Modifier::BOLD),
    )];
    for (i, (step, id)) in steps.iter().enumerate() {
        let on = *step == app.step;
        spans.push(Span::styled(format!("{:02} ", i + 1), th.fg(th.p.text_3)));
        let label = app.t(id, &[]).to_uppercase();
        let style = if on {
            th.fg(th.p.rescue).add_modifier(Modifier::BOLD)
        } else {
            th.fg(th.p.text_2)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw("   "));
    }
    let (files, _) = app.model.total_files();
    let search = if app.query.is_empty() && !matches!(app.mode, Mode::Search) {
        app.t("tui-search", &[("count", Arg::from(files))])
    } else {
        format!("/{}", app.query)
    };
    let [left, right] = Layout::horizontal([Constraint::Min(10), Constraint::Length(34)]).areas(area);
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(th.ground(th.p.ink_1)),
        left,
    );
    let style = if matches!(app.mode, Mode::Search) {
        th.on(th.p.text, th.p.ink_3)
    } else {
        th.on(th.p.text_3, th.p.ink_2)
    };
    f.render_widget(Paragraph::new(format!(" {search}")).style(style), right);
}

fn header(f: &mut Frame<'_>, app: &App, area: Rect) {
    let th = &app.theme;
    let mut facts = vec![Span::styled(
        format!(" {}  ", app.title),
        th.fg(th.p.text).add_modifier(Modifier::BOLD),
    )];
    for (k, v) in &app.facts {
        facts.push(Span::styled(format!("{k} "), th.fg(th.p.text_2)));
        facts.push(Span::styled(format!("{v}   "), th.fg(th.p.text)));
    }
    let lines = vec![
        Line::from(facts),
        strip(app, area.width.saturating_sub(2)),
        match app.notes.first() {
            Some(n) => Line::from(vec![
                Span::styled(" ● ", th.fg(th.p.info)),
                Span::styled(n.clone(), th.fg(th.p.text_2)),
            ]),
            None => Line::default(),
        },
    ];
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(th.fg(th.p.line));
    f.render_widget(
        Paragraph::new(lines).block(block).style(th.ground(th.p.ink_1)),
        area,
    );
}

/// The Bad-sector map as one bar: readable, unreadable, not read yet, and the read head.
fn strip(app: &App, width: u16) -> Line<'static> {
    let th = &app.theme;
    let w = u64::from(width.max(1));
    let size = app.source_size.max(1);
    let mut spans = vec![Span::raw(" ")];
    for i in 0..w {
        let a = i * size / w;
        let b = ((i + 1) * size / w).max(a + 1);
        let bad = app.bad.iter().any(|r| r.start < b && a < r.end);
        let read = app.read_until.is_none_or(|h| a < h);
        let head = app.read_until.is_some_and(|h| h >= a && h < b);
        let (glyph, color) = if head {
            ("▌", th.p.rescue)
        } else if bad {
            ("▆", th.p.bad)
        } else if read {
            ("▆", th.p.ok)
        } else {
            ("▆", th.p.ink_3)
        };
        let glyph = if th.depth == crate::theme::Depth::None && bad {
            "x"
        } else {
            glyph
        };
        spans.push(Span::styled(glyph, th.fg(color)));
    }
    Line::from(spans)
}

fn browse(f: &mut Frame<'_>, app: &App, area: Rect) {
    let (tree_w, insp_w) = match area.width {
        0..=69 => (0, 0),
        70..=109 => (26, 0),
        _ => (28, 34),
    };
    let [tree_area, files_area, insp_area] = Layout::horizontal([
        Constraint::Length(tree_w),
        Constraint::Min(20),
        Constraint::Length(insp_w),
    ])
    .areas(area);
    if tree_w > 0 {
        tree(f, app, tree_area);
    }
    files(f, app, files_area);
    if insp_w > 0 {
        inspector(f, app, insp_area);
    }
}

fn tree(f: &mut Frame<'_>, app: &App, area: Rect) {
    let th = &app.theme;
    let rows = app.tree_rows();
    let h = usize::from(area.height.saturating_sub(1));
    let first = app.tree_sel.saturating_sub(h.saturating_sub(1));
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(first)
        .take(h)
        .map(|(i, r)| {
            let arrow = if !r.expandable {
                " "
            } else if r.expanded {
                "▾"
            } else {
                "▸"
            };
            let text = format!("{}{arrow} {}", "  ".repeat(usize::from(r.depth)), r.label);
            let mut style = if r.lost {
                th.fg(th.p.warn)
            } else {
                th.fg(th.p.text_2)
            };
            if r.deleted {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if i == app.tree_sel {
                style = if app.focus == Focus::Tree {
                    th.selected()
                } else {
                    th.on(th.p.text, th.p.ink_2)
                };
            }
            Line::styled(text, style)
        })
        .collect();
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(th.fg(th.p.line));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn files(f: &mut Frame<'_>, app: &App, area: Rect) {
    let th = &app.theme;
    let list = app.current_files();
    let header = Row::new(
        [
            "",
            "tui-col-name",
            "tui-col-size",
            "tui-col-modified",
            "tui-col-assembly",
            "tui-col-integrity",
        ]
        .map(|k| {
            Cell::from(if k.is_empty() {
                String::new()
            } else {
                app.t(k, &[]).to_uppercase()
            })
        }),
    )
    .style(th.fg(th.p.text_3));
    let h = usize::from(area.height.saturating_sub(1));
    let first = app.file_sel.saturating_sub(h.saturating_sub(1));
    let rows: Vec<Row> = list
        .iter()
        .enumerate()
        .skip(first)
        .take(h)
        .map(|(i, e)| {
            let x = &e.entry;
            let (label, kind) = chip(x.assembly);
            let chip_style = match kind {
                ChipKind::Neutral => th.on(th.p.text_2, th.p.ink_2),
                ChipKind::Mirror => th.on(th.p.info, th.p.ink_2),
                ChipKind::Guess => th.on(th.p.warn, th.p.ink_2),
                ChipKind::Carved => th.fg(th.p.text_3).add_modifier(Modifier::ITALIC),
            };
            let mark = app.integrity_of(e.id.0);
            let mark_color = match mark {
                Mark::Intact => th.p.ok,
                Mark::Partial => th.p.warn,
                Mark::Damaged => th.p.bad,
                Mark::Unverified => th.p.text_3,
            };
            let marked = if app.model.marked.contains(&e.id.0) {
                "◆"
            } else {
                " "
            };
            let mut name_style = th.fg(th.p.text);
            if x.state == EntryState::Deleted {
                name_style = name_style.add_modifier(Modifier::ITALIC);
            }
            let row = Row::new(vec![
                Cell::from(Span::styled(marked, th.fg(th.p.rescue))),
                Cell::from(Span::styled(x.name.clone(), name_style)),
                Cell::from(Line::from(size(x.size)).alignment(Alignment::Right)).style(th.fg(th.p.text_2)),
                Cell::from(
                    x.times
                        .modified
                        .map(|t| short_time(&t.to_rfc3339()))
                        .unwrap_or_default(),
                )
                .style(th.fg(th.p.text_2)),
                Cell::from(Span::styled(format!(" {label} "), chip_style)),
                Cell::from(Line::from(vec![
                    Span::styled(format!("{} ", mark.glyph()), th.fg(mark_color)),
                    Span::styled(app.t(mark.word(), &[]), th.fg(th.p.text_2)),
                ])),
            ]);
            if i == app.file_sel {
                let sel = if app.focus == Focus::Files {
                    th.on(
                        th.p.text,
                        rescyo_design::Rgba {
                            a: 0x38,
                            ..th.p.rescue
                        },
                    )
                } else {
                    th.on(th.p.text, th.p.ink_2)
                };
                row.style(sel)
            } else {
                row
            }
        })
        .collect();
    let widths = [
        Constraint::Length(1),
        Constraint::Min(16),
        Constraint::Length(10),
        Constraint::Length(16),
        Constraint::Length(13),
        Constraint::Length(13),
    ];
    let table = Table::new(rows, widths).header(header).column_spacing(1);
    f.render_widget(table, area);
    if list.is_empty() {
        let msg = Paragraph::new(app.t("tui-empty", &[]))
            .style(th.fg(th.p.text_3))
            .alignment(Alignment::Center);
        let r = Rect {
            y: area.y + area.height / 2,
            height: 1,
            ..area
        };
        f.render_widget(msg, r);
    }
}

/// `2023-07-14T18:02:00Z` → `2023-07-14 18:02`.
fn short_time(s: &str) -> String {
    s.get(..16)
        .map(|t| t.replace('T', " "))
        .unwrap_or_else(|| s.to_owned())
}

fn inspector(f: &mut Frame<'_>, app: &App, area: Rect) {
    let th = &app.theme;
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(th.fg(th.p.line));
    let Some(e) = app.current_file() else {
        f.render_widget(block, area);
        return;
    };
    let x = &e.entry;
    let disk = x
        .layout
        .extents
        .iter()
        .filter(|e| matches!(e, Extent::Disk { .. }))
        .count();
    let (label, _) = chip(x.assembly);
    let mark = app.integrity_of(e.id.0);
    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<12}"), th.fg(th.p.text_3)),
            Span::styled(v, th.fg(th.p.text)),
        ])
    };
    let mut lines = vec![
        Line::styled(x.name.clone(), th.fg(th.p.text).add_modifier(Modifier::BOLD)),
        Line::default(),
        kv(&app.t("tui-col-assembly", &[]), label.to_lowercase()),
        kv(
            &app.t("tui-col-integrity", &[]),
            format!("{} {}", mark.glyph(), app.t(mark.word(), &[])),
        ),
        kv(
            &app.t("tui-col-size", &[]),
            format!("{} ({})", size(x.size), x.size),
        ),
        kv(&app.t("tui-insp-runs", &[]), disk.to_string()),
        kv(
            &app.t("tui-insp-state", &[]),
            app.t(
                if x.state == EntryState::Deleted {
                    "tui-deleted"
                } else {
                    "tui-live"
                },
                &[],
            ),
        ),
    ];
    if x.kind == EntryKind::Stream {
        lines.push(kv(&app.t("tui-insp-kind", &[]), app.t("tui-stream", &[])));
    }
    if let Some(p) = &app.current_path {
        lines.push(Line::default());
        lines.push(Line::styled(p.clone(), th.fg(th.p.text_3)));
    }
    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn scan_screen(f: &mut Frame<'_>, app: &App, area: Rect) {
    let th = &app.theme;
    let pct = app
        .scan
        .total
        .filter(|&t| t > 0)
        .map(|t| app.scan.done.min(t) * 100 / t);
    let lines = vec![
        Line::default(),
        Line::styled(
            app.t("tui-scanning", &[("source", Arg::from(app.title.as_str()))]),
            th.fg(th.p.text).add_modifier(Modifier::BOLD),
        ),
        Line::default(),
        Line::styled(app.t(&app.scan.phase, &[]), th.fg(th.p.text_2)),
        Line::styled(
            match pct {
                Some(p) => format!(
                    "{p}%  {} / {}",
                    size(app.scan.done),
                    size(app.scan.total.unwrap_or(0))
                ),
                None => app.scan.done.to_string(),
            },
            th.fg(th.p.text_3),
        ),
    ];
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

fn safety_bar(f: &mut Frame<'_>, app: &App, area: Rect) {
    let th = &app.theme;
    f.render_widget(Block::default().style(th.ground(th.p.ink_0)), area);
    let target = app
        .target
        .as_ref()
        .map(|t| t.display().to_string())
        .unwrap_or_else(|| app.t("tui-no-target", &[]));
    let (pf, pf_color, glyph) = match &app.preflight {
        Preflight::Unknown => (String::new(), th.p.text_3, ""),
        Preflight::Ok { free } => (
            app.t("tui-preflight-ok", &[("free", Arg::from(size(*free)))]),
            th.p.text_2,
            "✓ ",
        ),
        Preflight::Problems(p) => (
            app.t(
                "tui-preflight-problems",
                &[
                    ("count", Arg::from(p.len() as u64)),
                    ("first", Arg::from(p.first().map_or("", String::as_str))),
                ],
            ),
            th.p.bad,
            "✕ ",
        ),
    };
    let count = app.recover_count();
    let button = format!(
        " {} ",
        app.t("tui-recover-button", &[("count", Arg::from(count))])
    );
    let bw = u16::try_from(button.chars().count()).unwrap_or(20);
    let [left, right] = Layout::horizontal([Constraint::Min(10), Constraint::Length(bw + 1)]).areas(area);
    let lines = vec![
        Line::from(vec![
            Span::styled(
                format!(" 04 {}  ", app.t("tui-recover-to", &[]).to_uppercase()),
                th.fg(th.p.text_3),
            ),
            Span::styled(target, th.fg(th.p.text).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled(
                format!(" {glyph}"),
                th.fg(if matches!(app.preflight, Preflight::Ok { .. }) {
                    th.p.ok
                } else {
                    pf_color
                }),
            ),
            Span::styled(pf, th.fg(pf_color)),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), left);
    let ready = matches!(app.preflight, Preflight::Ok { .. }) && count > 0;
    let style = if ready {
        th.primary()
    } else {
        th.on(th.p.text_3, th.p.ink_2)
    };
    f.render_widget(
        Paragraph::new(Line::styled(button, style)),
        Rect { height: 1, ..right },
    );
}

fn overlay(f: &mut Frame<'_>, app: &App, area: Rect) {
    let th = &app.theme;
    let (title, body): (String, Vec<Line>) = match &app.mode {
        Mode::Input(text) => (
            app.t("tui-input-target", &[]),
            vec![
                Line::styled(format!("{text}▏"), th.fg(th.p.text)),
                Line::default(),
                Line::styled(app.t("tui-input-hint", &[]), th.fg(th.p.text_3)),
            ],
        ),
        Mode::Running { done, total } => (
            app.t("tui-step-recover", &[]),
            vec![Line::styled(
                app.t(
                    "tui-running",
                    &[
                        ("done", Arg::from(size(*done))),
                        ("total", Arg::from(size(*total))),
                    ],
                ),
                th.fg(th.p.text),
            )],
        ),
        Mode::Message(lines) => (
            String::new(),
            lines
                .iter()
                .map(|l| Line::styled(l.clone(), th.fg(th.p.text)))
                .collect(),
        ),
        Mode::Help => (
            app.t("tui-help-title", &[]),
            app.t("tui-help", &[])
                .lines()
                .map(|l| Line::styled(l.to_owned(), th.fg(th.p.text_2)))
                .collect(),
        ),
        Mode::Normal | Mode::Search => return,
    };
    let w = area.width.saturating_sub(8).min(72);
    let h = u16::try_from(body.len() + 2).unwrap_or(5).min(area.height);
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    let block = Block::default()
        .title(Span::styled(
            format!(" {title} "),
            th.fg(th.p.text).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(th.fg(th.p.rescue))
        .style(th.ground(th.p.ink_2));
    f.render_widget(Paragraph::new(body).block(block).wrap(Wrap { trim: false }), r);
}
