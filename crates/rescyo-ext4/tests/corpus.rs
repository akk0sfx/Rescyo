//! The ext4 scenario of the corpus, built through the Linux kernel driver: every live file byte
//! for byte with its path and modification time, also with the primary superblock destroyed;
//! deleted files are reported (experimental tier: the kernel may leave nothing to recover).
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "integration test"
)]

use std::collections::HashMap;

use rescyo_fs::{CancelToken, EntryId, EntryKind, EntryState, Limits, VecSink};
use rescyo_io::{GuardedReader, ReadPolicy};
use rescyo_testkit::{FileState, require_corpus};
use sha2::{Digest, Sha256};

fn read(r: &GuardedReader, layout: &rescyo_fs::DataLayout, size: u64) -> Vec<u8> {
    let mut out = Vec::new();
    rescyo_fs::content::stream(r, layout, size, &mut |d, _| {
        out.extend_from_slice(d);
        Ok(())
    })
    .unwrap();
    out
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn ext4_live_files_are_exact_and_deleted_ones_reported() {
    let _ = require_corpus!();
    for s in rescyo_testkit::scenarios()
        .into_iter()
        .filter(|s| s.expected.fs == "ext4")
    {
        let e = &s.expected;
        for corruption in &e.corruptions {
            let r = GuardedReader::new(s.open(corruption).unwrap(), ReadPolicy::default());
            let found = rescyo_ext4::probe(&r, e.partition.offset).unwrap();
            let g = &found
                .first()
                .unwrap_or_else(|| panic!("{corruption}: no superblock"))
                .geometry;
            let mut sink = VecSink::default();
            rescyo_ext4::walk(&r, g, &Limits::default(), &mut sink, &CancelToken::new()).unwrap();
            let by_path: HashMap<String, &rescyo_fs::Entry> = sink
                .entries
                .iter()
                .enumerate()
                .filter(|(_, x)| x.kind == EntryKind::File)
                .map(|(i, x)| (sink.path_of(EntryId(i as u64 + 1)), x))
                .collect();
            for f in e.files.iter().filter(|f| f.state == FileState::Live) {
                let got = by_path
                    .get(&f.path)
                    .unwrap_or_else(|| panic!("{corruption}: missing {}", f.path));
                assert_eq!(got.state, EntryState::Live, "{}", f.path);
                assert_eq!(got.size, f.size, "{}", f.path);
                assert_eq!(
                    sha(&read(&r, &got.layout, got.size)),
                    f.sha256,
                    "{corruption}: {}",
                    f.path
                );
                assert_eq!(
                    got.times.modified.map(|t| t.unix),
                    Some(f.mtime),
                    "{} mtime",
                    f.path
                );
            }
            let deleted: Vec<_> = e.files.iter().filter(|f| f.state == FileState::Deleted).collect();
            let named = deleted.iter().filter(|f| by_path.contains_key(&f.path)).count();
            let exact = deleted
                .iter()
                .filter(|f| {
                    by_path
                        .get(&f.path)
                        .is_some_and(|x| sha(&read(&r, &x.layout, x.size)) == f.sha256)
                })
                .count();
            eprintln!(
                "{} {corruption}: deleted names {named}/{}, content {exact}/{}",
                e.scenario,
                deleted.len(),
                deleted.len()
            );
        }
    }
}
