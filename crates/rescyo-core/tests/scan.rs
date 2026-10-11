//! Scans every corpus scenario into a real Session and checks the stored entries.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "integration test"
)]

use std::collections::HashMap;

use rescyo_core::{ScanMode, ScanOptions, Session, scan};
use rescyo_fs::{CancelToken, EntryKind, EntryState, Extent};
use rescyo_io::{FingerprintMatch, GuardedReader, ReadPolicy, SourceFingerprint};
use rescyo_testkit::{FileState, require_corpus};
use sha2::{Digest, Sha256};

fn read_layout(r: &GuardedReader, extents: &[Extent]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in extents {
        match e {
            Extent::Disk { offset, len } => {
                let mut b = vec![0u8; *len as usize];
                r.read(*offset, &mut b).unwrap();
                out.extend_from_slice(&b);
            }
            Extent::Zero { len } => out.resize(out.len() + *len as usize, 0),
            Extent::Inline { data } => out.extend_from_slice(data),
        }
    }
    out
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn every_scenario_scans_into_a_session_with_paths_and_content() {
    for s in require_corpus!() {
        let e = &s.expected;
        let dir = tempfile::tempdir().unwrap();
        let src = s.open("none").unwrap();
        let r = GuardedReader::new(src, ReadPolicy::default());
        let fp = SourceFingerprint::compute(&r).unwrap();
        let mut session = Session::create(&dir.path().join("s.db"), r.identity(), &fp).unwrap();
        let report = scan(
            &r,
            &mut session,
            &ScanOptions::default(),
            &mut |_| {},
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(
            report.volumes.iter().filter(|v| v.walked).count(),
            1,
            "{}: {report:?}",
            e.scenario
        );
        drop(session);

        // Reopen from disk: everything must have been committed.
        let session = Session::open(&dir.path().join("s.db")).unwrap();
        assert_eq!(session.check_source(&fp).unwrap(), FingerprintMatch::Match);
        let entries = session.entries(|x| x.entry.kind == EntryKind::File).unwrap();
        let by_path: HashMap<String, _> = entries
            .iter()
            .map(|x| (session.path_of(x.id).unwrap(), x))
            .collect();
        for f in e.files.iter().filter(|f| f.state == FileState::Live) {
            let got = by_path
                .get(&f.path)
                .unwrap_or_else(|| panic!("{}: missing {}", e.scenario, f.path));
            assert_eq!(got.entry.state, EntryState::Live);
            assert_eq!(
                sha(&read_layout(&r, &got.entry.layout.extents)),
                f.sha256,
                "{}: {}",
                e.scenario,
                f.path
            );
            assert_eq!(
                got.entry.times.modified.map(|t| t.unix),
                Some(f.mtime),
                "{}: {}",
                e.scenario,
                f.path
            );
        }
    }
}

#[test]
fn deep_scan_finds_volumes_without_any_boot_sector() {
    for s in require_corpus!() {
        let e = &s.expected;
        let worst = e
            .corruptions
            .iter()
            .max_by_key(|c| c.split('+').count())
            .cloned()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let r = GuardedReader::new(s.open(&worst).unwrap(), ReadPolicy::default());
        let fp = SourceFingerprint::compute(&r).unwrap();
        let mut session = Session::create(&dir.path().join("s.db"), r.identity(), &fp).unwrap();
        let opts = ScanOptions {
            mode: ScanMode::Deep,
            ..ScanOptions::default()
        };
        let report = scan(&r, &mut session, &opts, &mut |_| {}, &CancelToken::new()).unwrap();
        let files = session.entries(|x| x.entry.kind == EntryKind::File).unwrap();
        let shas: std::collections::HashSet<String> = files
            .iter()
            .map(|x| sha(&read_layout(&r, &x.entry.layout.extents)))
            .collect();
        // A file Rescyo itself labelled a contiguous guess is an honest miss, not a failure.
        let guessed: std::collections::HashSet<&str> = files
            .iter()
            .filter(|x| x.entry.assembly.is_guess())
            .map(|x| x.entry.name.as_str())
            .collect();
        let live: Vec<_> = e
            .files
            .iter()
            .filter(|f| f.state == FileState::Live && !f.in_root)
            .filter(|f| {
                !(f.fragmented != Some(false) && guessed.contains(f.path.rsplit('/').next().unwrap_or("")))
            })
            .collect();
        let found = live.iter().filter(|f| shas.contains(&f.sha256)).count();
        assert_eq!(
            found,
            live.len(),
            "{} {worst}: {found}/{} ({report:?})",
            e.scenario,
            live.len()
        );
    }
}

#[test]
fn sessions_refuse_overwrite_and_foreign_files() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.db");
    std::fs::write(&p, b"not a session").unwrap();
    assert!(Session::open(&p).is_err());
    let r = GuardedReader::new(
        std::sync::Arc::new(rescyo_io::testing::MemSource::new(vec![0; 1 << 16], 512)),
        ReadPolicy::default(),
    );
    let fp = SourceFingerprint::compute(&r).unwrap();
    assert!(Session::create(&p, r.identity(), &fp).is_err());
}
