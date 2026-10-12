#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rescyo_core::{ScanOptions, Session, VolumeRow};
use rescyo_fs::{
    AssemblyMethod, CancelToken, DataLayout, Entry, EntryId, EntryKind, EntryState, Evidence, FsKind, Parent,
    Times, VolumeCandidate,
};

use super::*;

fn entry(
    id: u64,
    parent: Parent,
    name: &str,
    kind: EntryKind,
    state: EntryState,
    a: AssemblyMethod,
) -> rescyo_core::StoredEntry {
    rescyo_core::StoredEntry {
        id: EntryId(id),
        volume: 1,
        entry: Entry {
            parent,
            name: name.into(),
            name_raw: None,
            kind,
            state,
            size: 4_404_019,
            times: Times::default(),
            layout: DataLayout::default(),
            assembly: a,
            fs_ref: id,
            seed: None,
        },
    }
}

fn model() -> Model {
    let vol = VolumeRow {
        id: 1,
        candidate: VolumeCandidate {
            fs: FsKind::Fat32,
            offset: Some(1 << 20),
            cluster_size: 16384,
            data_offset: 0x0100_0000,
            size: None,
            label: Some("UDISK".into()),
            evidence: vec![Evidence::BootSector { offset: 1 << 20 }],
            score: 1000,
            params: serde_json::Value::Null,
        },
        walked: true,
    };
    let d = |id, name| {
        entry(
            id,
            Parent::Root,
            name,
            EntryKind::Directory,
            EntryState::Live,
            AssemblyMethod::FatChain,
        )
    };
    let f = |id, p: u64, name, st, a| entry(id, Parent::Entry(EntryId(p)), name, EntryKind::File, st, a);
    Model::from_parts(
        vec![vol],
        vec![
            d(1, "Фото лето 2023"),
            d(2, "Документы"),
            f(3, 1, "IMG_2041.JPG", EntryState::Live, AssemblyMethod::FatChain),
            f(
                4,
                1,
                "IMG_2043.JPG",
                EntryState::Live,
                AssemblyMethod::FatChainMirror,
            ),
            f(
                5,
                1,
                "IMG_2050.JPG",
                EntryState::Deleted,
                AssemblyMethod::ContiguousAssumed,
            ),
            f(6, 2, "Баланс.xlsx", EntryState::Live, AssemblyMethod::FatChain),
            entry(
                7,
                Parent::Lost { parent_ref: 99 },
                "orphan",
                EntryKind::Directory,
                EntryState::Live,
                AssemblyMethod::FatChain,
            ),
        ],
    )
}

fn app(depth: Depth) -> App {
    let mut a = App::new(
        Theme::new(true, depth),
        Localizer::new(Some("en")),
        model(),
        "General UDisk".into(),
    );
    a.source_size = 31_400_000_000;
    a
}

fn render(a: &App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| ui::draw(f, a)).unwrap();
    let buf = t.backend().buffer().clone();
    let mut s = String::new();
    for y in 0..h {
        for x in 0..w {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

fn key(a: &mut App, c: KeyCode) -> Action {
    a.key(KeyEvent::new(c, KeyModifiers::NONE))
}

#[test]
fn the_browse_screen_follows_the_design() {
    let mut a = app(Depth::TrueColor);
    key(&mut a, KeyCode::Tab); // focus the tree
    key(&mut a, KeyCode::Down); // "Документы" sorts before "Фото…"
    key(&mut a, KeyCode::Down);
    key(&mut a, KeyCode::Tab);
    let s = render(&a, 150, 30);
    for want in [
        "01 SOURCE",
        "02 SCAN",
        "03 BROWSE",
        "04 RECOVER",
        "General UDisk",
        "Фото лето 2023",
        "_LOST_FOLDERS",
    ] {
        assert!(s.contains(want), "missing {want:?}\n{s}");
    }
    for want in [
        "IMG_2041.JPG",
        "FAT CHAIN",
        "MIRROR FAT",
        "CONTIGUOUS?",
        "○ Unverified",
        "4.2 MiB",
    ] {
        assert!(s.contains(want), "missing {want:?}\n{s}");
    }
    assert!(s.contains("Recover 3 files"), "{s}");
}

#[test]
fn narrow_terminals_drop_the_inspector_then_the_tree() {
    let a = app(Depth::Indexed);
    assert!(!render(&a, 80, 24).contains("Fragments"));
    let s = render(&a, 60, 20);
    assert!(!s.contains("_LOST_FOLDERS"), "{s}");
}

#[test]
fn marking_searching_and_hiding_deleted() {
    let mut a = app(Depth::None);
    key(&mut a, KeyCode::Tab);
    key(&mut a, KeyCode::Down);
    key(&mut a, KeyCode::Down);
    key(&mut a, KeyCode::Tab);
    assert_eq!(key(&mut a, KeyCode::Char(' ')), Action::Preflight);
    assert_eq!(a.recover_ids().len(), 1);
    key(&mut a, KeyCode::Char('d'));
    assert!(
        !a.model
            .files(rescyo_core::browse::Node::Dir(1), "")
            .iter()
            .any(|e| e.entry.name == "IMG_2050.JPG")
    );
    key(&mut a, KeyCode::Char('/'));
    for c in "балан".chars() {
        key(&mut a, KeyCode::Char(c));
    }
    key(&mut a, KeyCode::Enter);
    assert_eq!(
        a.current_files()
            .iter()
            .map(|e| e.entry.name.as_str())
            .collect::<Vec<_>>(),
        ["Баланс.xlsx"]
    );
}

#[test]
fn every_language_renders_without_missing_messages() {
    for lang in ["en", "ru", "zh-CN", "ja", "ko"] {
        let mut a = app(Depth::TrueColor);
        a.l10n = Localizer::new(Some(lang));
        let s = render(&a, 150, 30);
        assert!(!s.contains("tui-"), "{lang}: untranslated id\n{s}");
    }
}

#[test]
fn a_real_session_scans_and_renders() {
    let Some(sc) = rescyo_testkit_like() else { return };
    let dir = tempfile::tempdir().unwrap();
    let r = rescyo_io::GuardedReader::new(
        std::sync::Arc::new(rescyo_io::ImageSource::open(&sc, 512).unwrap()),
        rescyo_io::ReadPolicy::default(),
    );
    let fp = rescyo_io::SourceFingerprint::compute(&r).unwrap();
    let mut s = Session::create(&dir.path().join("s.db"), r.identity(), &fp).unwrap();
    rescyo_core::scan(
        &r,
        &mut s,
        &ScanOptions::default(),
        &mut |_| {},
        &CancelToken::new(),
    )
    .unwrap();
    let mut a = new_app(
        &s,
        &Options {
            lang: Some("en".into()),
            dark: false,
        },
        "fat16".into(),
    )
    .unwrap();
    a.current_path = None;
    let out = render(&a, 150, 40);
    assert!(out.contains("FAT16"), "{out}");
    assert!(out.contains("folders"), "{out}");
}

fn rescyo_testkit_like() -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/out/fat16-mbr/image.img");
    p.exists().then_some(p)
}
