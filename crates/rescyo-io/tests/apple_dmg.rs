//! Disk images written by hdiutil (corpus/apple/build.sh): every format must read back exactly
//! the volume it was made from. Runs only where RESCYO_APPLE_IMAGES names the built corpus.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "integration test")]

use std::path::PathBuf;

use rescyo_io::{ContainerFormat, open_image};

fn corpus() -> Option<PathBuf> {
    std::env::var_os("RESCYO_APPLE_IMAGES").map(PathBuf::from)
}

#[test]
fn every_udif_format_reads_back_the_source_volume() {
    let Some(dir) = corpus() else {
        eprintln!("RESCYO_APPLE_IMAGES not set; skipping");
        return;
    };
    let want = std::fs::read(dir.join("hfs.img")).unwrap();
    for kind in ["UDRO", "UDCO", "UDZO", "UDBZ", "ULFO", "ULMO"] {
        let path = dir.join(format!("hfs-{kind}.dmg"));
        let (src, info) = open_image(&path, 512, None).unwrap();
        assert_eq!(info.format, ContainerFormat::Dmg, "{kind}");
        assert!(info.notes.is_empty(), "{kind}: {:?}", info.notes);
        let mut got = vec![0u8; usize::try_from(src.geometry().size).unwrap()];
        src.read_at(0, &mut got).unwrap();
        assert_eq!(got.len(), want.len(), "{kind}");
        assert!(got == want, "{kind}: virtual disk differs from hfs.img");
    }
}
