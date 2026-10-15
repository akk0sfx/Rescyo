#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "tests"
)]

use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{Fact, FileRow, Kv, L, Look, MainWindow, SourceItem, Tokens, TreeItem};

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

thread_local! {
    static WINDOW: Rc<MinimalSoftwareWindow> = {
        let w = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Offscreen(w.clone()))).unwrap();
        w
    };
}

fn draw(w: u32, h: u32, show: impl FnOnce() -> Box<dyn FnOnce()>) -> Vec<PremultipliedRgbaColor> {
    let window = WINDOW.with(Clone::clone);
    let hide = show();
    window.set_size(slint::PhysicalSize::new(w, h));
    let mut buf = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
    window.draw_if_needed(|r| {
        r.render(&mut buf, w as usize);
    });
    hide();
    buf
}

fn save_png(path: &std::path::Path, w: u32, h: u32, buf: &[PremultipliedRgbaColor]) {
    let file = std::fs::File::create(path).unwrap();
    let mut e = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    let mut wr = e.write_header().unwrap();
    let data: Vec<u8> = buf.iter().flat_map(|p| [p.red, p.green, p.blue, 255]).collect();
    wr.write_image_data(&data).unwrap();
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

/// The example data of the design mockup (the flash drive that started the project).
pub fn mockup(ui: &MainWindow) {
    let s = |x: &str| SharedString::from(x);
    ui.set_sources(model(vec![
        SourceItem {
            name: s("General UDisk"),
            detail: s("31.4 GB · USB · FAT32"),
            kind: 0,
            group: 0,
            locked: false,
        },
        SourceItem {
            name: s("Macintosh HD"),
            detail: s("system disk · protected"),
            kind: 1,
            group: 0,
            locked: true,
        },
        SourceItem {
            name: s("flash-2026-10-11.img"),
            detail: s("29.3 GiB · 14 bad sectors"),
            kind: 2,
            group: 1,
            locked: false,
        },
        SourceItem {
            name: s("flash.db"),
            detail: s("imported from DMDE"),
            kind: 3,
            group: 2,
            locked: false,
        },
    ]));
    ui.set_source_groups(model(vec![true, true, true]));
    ui.set_heading(s("General UDisk"));
    ui.set_step(2);
    ui.set_facts(model(vec![
        Fact {
            label: s(""),
            value: s("FAT32"),
        },
        Fact {
            label: s("cluster"),
            value: s("16 KiB"),
        },
        Fact {
            label: s("data at"),
            value: s("0x01000000"),
        },
        Fact {
            label: s(""),
            value: s("476 folders"),
        },
        Fact {
            label: s(""),
            value: s("1 284 files · 7.9 GiB"),
        },
    ]));
    // A damaged band and scattered bad sectors, read up to 72% (the mockup's strip).
    let mut seed: u64 = 7;
    let mut rnd = || {
        seed = (seed * 16807) % 2_147_483_647;
        seed as f64 / 2_147_483_647.0
    };
    let n = 220;
    let cells: Vec<i32> = (0..n)
        .map(|i| {
            if f64::from(i) > 0.72 * f64::from(n) {
                0
            } else if (f64::from(i) > f64::from(n) * 0.31 && f64::from(i) < f64::from(n) * 0.34)
                || rnd() < 0.025
            {
                2
            } else {
                1
            }
        })
        .collect();
    ui.set_strip(model(cells));
    ui.set_head(0.72);
    ui.set_note_title(s("Boot sector damaged."));
    ui.set_note(s(
        "Both FAT copies found by signature; 42 folders whose parent is lost were placed in _LOST_FOLDERS.",
    ));
    let t = |label: &str, depth, expandable, expanded, lost| TreeItem {
        label: s(label),
        depth,
        expandable,
        expanded,
        lost,
        deleted: false,
    };
    ui.set_tree(model(vec![
        t("General UDisk", 0, true, true, false),
        t("DCIM", 1, true, false, false),
        t("Фото лето 2023", 1, true, true, false),
        t("Пляж", 2, false, false, false),
        t("Горы", 2, false, false, false),
        t("Документы", 1, true, false, false),
        t("写真", 1, true, false, false),
        t("_LOST_FOLDERS", 1, true, false, true),
    ]));
    ui.set_tree_index(2);
    let f = |name: &str, size: &str, m: &str, chip: &str, ck, integ| FileRow {
        name: s(name),
        size: s(size),
        modified: s(m),
        chip: s(chip),
        chip_kind: ck,
        integrity: integ,
        marked: false,
        deleted: false,
    };
    ui.set_files(model(vec![
        f("IMG_2041.JPG", "4.2 MiB", "2023-07-14 18:02", "FAT CHAIN", 0, 0),
        f("IMG_2042.JPG", "3.9 MiB", "2023-07-14 18:03", "FAT CHAIN", 0, 0),
        f("IMG_2043.JPG", "4.4 MiB", "2023-07-14 18:05", "MIRROR FAT", 1, 0),
        f("VID_0031.MP4", "612 MiB", "2023-07-15 11:40", "FAT CHAIN", 0, 1),
        f(
            "Отчёт за 2023 год.docx",
            "48 KiB",
            "2023-12-28 09:15",
            "FAT CHAIN",
            0,
            0,
        ),
        f("Баланс.xlsx", "112 KiB", "2023-12-29 16:41", "CONTIGUOUS?", 2, 2),
        f("IMG_2050.JPG", "4.0 MiB", "2023-07-16 08:12", "CONTIGUOUS?", 2, 0),
        f("notes.txt", "2 KiB", "2023-08-01 20:30", "FAT CHAIN", 0, 3),
    ]));
    ui.set_file_index(0);
    ui.set_inspector_name(s("IMG_2041.JPG"));
    let kv = |k: &str, v: &str, mono| Kv {
        key: s(k),
        value: s(v),
        mono,
    };
    ui.set_inspector(model(vec![
        kv("Assembly", "FAT chain (primary)", false),
        kv("Integrity", "Intact · JPEG decoded", false),
        kv("Clusters", "269 in 2 runs", true),
        kv("Unreadable", "none", false),
        kv("SHA-256", "3f2a9c…77c91e", true),
    ]));
    ui.set_target(s("/Volumes/Backup/FROM_FLASH"));
    ui.set_preflight_ok(true);
    ui.set_preflight(s("Preflight: different device · 212 GiB free · names OK"));
    ui.set_recover_count(1284);
    ui.global::<L>().set_recover_button(s("Recover 1 284 files"));
    ui.set_search_placeholder(s("Search 1 284 files…"));
}

fn render_main(dark: bool, mac: bool) -> Vec<PremultipliedRgbaColor> {
    draw(1360, 820, || {
        let ui = MainWindow::new().unwrap();
        ui.global::<Tokens>().set_dark(dark);
        ui.global::<Look>().set_mac(mac);
        mockup(&ui);
        ui.show().unwrap();
        Box::new(move || ui.hide().unwrap())
    })
}

#[test]
fn the_main_window_renders_the_mockup() {
    let out = std::env::var_os("RESCYO_GUI_SHOTS").map(std::path::PathBuf::from);
    for (dark, mac, name) in [
        (true, true, "mac-dark"),
        (false, false, "win-light"),
        (true, false, "win-dark"),
    ] {
        let buf = render_main(dark, mac);
        let lit = buf
            .iter()
            .filter(|p| u32::from(p.red) + u32::from(p.green) + u32::from(p.blue) > 0)
            .count();
        assert!(lit > 100_000, "{name}: nothing drawn");
        if let Some(dir) = &out {
            std::fs::create_dir_all(dir).unwrap();
            save_png(&dir.join(format!("{name}.png")), 1360, 820, &buf);
        }
    }
}

#[test]
fn variable_fonts_render_their_weights_deterministically() {
    let probe = |weight| {
        draw(320, 120, || {
            let ui = crate::Probe::new().unwrap();
            ui.set_weight(weight);
            ui.show().unwrap();
            Box::new(move || ui.hide().unwrap())
        })
    };
    let ink = |b: &[PremultipliedRgbaColor]| b[..320 * 50].iter().filter(|p| p.red > 128).count();
    let px = |b: &[PremultipliedRgbaColor]| {
        b.iter()
            .map(|p| (p.red, p.green, p.blue, p.alpha))
            .collect::<Vec<_>>()
    };
    let regular = probe(400);
    let bold = probe(700);
    let again = probe(400);
    assert!(ink(&regular) > 200, "no text drawn");
    assert!(ink(&bold) > ink(&regular) * 11 / 10, "weight axis ignored");
    assert!(px(&regular) == px(&again), "rendering is not deterministic");
}

#[test]
fn a_real_session_drives_the_window() {
    let img =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/out/fat16-mbr/image.img");
    if !img.exists() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("rescyo-gui-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let session = dir.join("s.rescyo");
    let _ = std::fs::remove_file(&session);
    let r = crate::controller::open_source(img.to_str().unwrap()).unwrap();
    let fp = rescyo_io::SourceFingerprint::compute(&r).unwrap();
    let mut s = rescyo_core::Session::create(&session, r.identity(), &fp).unwrap();
    rescyo_core::scan(
        &r,
        &mut s,
        &rescyo_core::ScanOptions::default(),
        &mut |_| {},
        &rescyo_fs::CancelToken::new(),
    )
    .unwrap();
    drop(s);
    let out = std::env::var_os("RESCYO_GUI_SHOTS").map(std::path::PathBuf::from);
    for (lang, dark) in [("ru", true), ("ja", false)] {
        let buf = draw(1360, 820, || {
            let ui = MainWindow::new().unwrap();
            ui.global::<Tokens>().set_dark(dark);
            crate::fonts::load_cjk(lang);
            let c = crate::controller::Controller::new(&ui, rescyo_i18n::Localizer::new(Some(lang)));
            c.open_session(&session);
            assert_eq!(ui.get_step(), 2);
            assert!(!ui.get_heading().is_empty());
            assert!(slint::Model::row_count(&ui.get_tree()) > 3);
            ui.show().unwrap();
            Box::new(move || {
                drop(c);
                ui.hide().unwrap();
            })
        });
        if let Some(d) = &out {
            save_png(&d.join(format!("session-{lang}.png")), 1360, 820, &buf);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
