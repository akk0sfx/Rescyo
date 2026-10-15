//! CJK fonts. Han characters are unified in Unicode but drawn differently in China, Japan and
//! Korea, so the Noto Sans SC/JP/KR packs are registered as fallbacks in the order the interface
//! language calls for. The packs ship next to the binary (`fonts/`), not inside it; without
//! them the system's fonts are used.

use std::path::{Path, PathBuf};

use slint::fontique_011::{fontique, shared_collection};

const PACKS: [(&str, &str); 3] = [
    ("sc", "NotoSansSC[wght].ttf"),
    ("jp", "NotoSansJP[wght].ttf"),
    ("kr", "NotoSansKR[wght].ttf"),
];

/// Where language packs are looked for, in order.
fn dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(d) = std::env::var_os("RESCYO_FONTS") {
        out.push(PathBuf::from(d));
    }
    if let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        out.push(exe.join("fonts"));
        out.push(exe.join("../share/rescyo/fonts"));
        out.push(exe.join("../Resources/fonts"));
    }
    out.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/fonts"));
    out
}

/// Pack order for a language: its own script variant first.
fn order(lang: &str) -> [&'static str; 3] {
    match lang {
        "ja" => ["jp", "kr", "sc"],
        "ko" => ["kr", "jp", "sc"],
        _ => ["sc", "jp", "kr"],
    }
}

/// Registers whatever packs are installed; returns how many were found.
pub fn load_cjk(lang: &str) -> usize {
    let dirs = dirs();
    let mut collection = shared_collection();
    let mut families = Vec::new();
    for key in order(lang) {
        let Some((_, file)) = PACKS.iter().find(|(k, _)| *k == key) else {
            continue;
        };
        let Some(data) = dirs.iter().find_map(|d| std::fs::read(d.join(file)).ok()) else {
            continue;
        };
        let blob = fontique::Blob::new(std::sync::Arc::new(data));
        families.extend(
            collection
                .register_fonts(blob, None)
                .into_iter()
                .map(|(id, _)| id),
        );
    }
    for script in [*b"Hani", *b"Hira", *b"Kana", *b"Hang", *b"Bopo"] {
        let key = fontique::FallbackKey::new(fontique::Script::from_bytes(script), None);
        collection.append_fallbacks(key, families.iter().copied());
    }
    families.len()
}
