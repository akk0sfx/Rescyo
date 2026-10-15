//! HFS+ images built by macOS itself (`corpus/apple/build.sh`, run on a disposable macOS
//! runner or with the owner's permission): every live file byte for byte, and the file the
//! macOS driver deleted reported. Skipped when RESCYO_APPLE_IMAGES is not set.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "integration test"
)]

use std::collections::HashMap;
use std::sync::Arc;

use rescyo_fs::{CancelToken, EntryId, EntryKind, EntryState, Limits, VecSink};
use rescyo_io::{GuardedReader, ImageSource, ReadPolicy};
use sha2::{Digest, Sha256};

#[test]
fn hfs_image_from_macos() {
    let Some(dir) = std::env::var_os("RESCYO_APPLE_IMAGES").map(std::path::PathBuf::from) else {
        eprintln!("skipped: RESCYO_APPLE_IMAGES not set");
        return;
    };
    let expected: HashMap<String, serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("files.json")).unwrap()).unwrap();
    let r = GuardedReader::new(
        Arc::new(ImageSource::open(&dir.join("hfs.img"), 512).unwrap()),
        ReadPolicy::default(),
    );
    let found = rescyo_hfs::probe(&r, 0, Some(r.geometry().size)).unwrap();
    let g = &found.first().expect("volume header").geometry;
    let mut sink = VecSink::default();
    rescyo_hfs::walk(&r, g, &Limits::default(), &mut sink, &CancelToken::new()).unwrap();
    let by_path: HashMap<String, &rescyo_fs::Entry> = sink
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == EntryKind::File)
        .map(|(i, e)| (sink.path_of(EntryId(i as u64 + 1)), e))
        .collect();
    for (path, want) in &expected {
        let deleted = path.ends_with("deleted.bin");
        let Some(e) = by_path.get(path) else {
            assert!(
                deleted,
                "missing {path}; have {:?}",
                by_path.keys().collect::<Vec<_>>()
            );
            eprintln!("deleted file not found: {path}");
            continue;
        };
        let mut data = Vec::new();
        rescyo_fs::content::stream(&r, &e.layout, e.size, &mut |d, _| {
            data.extend_from_slice(d);
            Ok(())
        })
        .unwrap();
        let sha: String = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
        if deleted {
            assert_eq!(e.state, EntryState::Deleted, "{path}");
            eprintln!(
                "deleted {path}: content {}",
                if sha == want["sha256"] { "exact" } else { "differs" }
            );
        } else {
            assert_eq!(e.state, EntryState::Live, "{path}");
            assert_eq!(e.size, want["size"].as_u64().unwrap(), "{path}");
            assert_eq!(sha, want["sha256"].as_str().unwrap(), "{path}");
        }
    }
}
