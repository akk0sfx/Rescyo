//! Walks the NTFS scenarios of the test corpus and checks every file byte for byte.
#![allow(
    clippy::cast_possible_truncation,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration test helpers"
)]

use std::collections::HashMap;

use rescyo_fs::{CancelToken, EntryId, EntryKind, EntryState, Extent, Limits, VecSink};
use rescyo_io::{GuardedReader, ReadPolicy};
use rescyo_ntfs::{NtfsGeometry, deep_scan, infer_volumes, probe_boot, walk};
use rescyo_testkit::{FileState, Scenario, require_corpus};
use sha2::{Digest, Sha256};

fn read_layout(r: &GuardedReader, extents: &[Extent]) -> Vec<u8> {
    let layout = rescyo_fs::DataLayout {
        extents: extents.to_vec(),
    };
    let mut out = Vec::new();
    rescyo_fs::content::stream(r, &layout, layout.len(), &mut |d, _| {
        out.extend_from_slice(d);
        Ok(())
    })
    .unwrap();
    out
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

fn recover(s: &Scenario, corruption: &str) -> (GuardedReader, NtfsGeometry, VecSink) {
    let r = GuardedReader::new(s.open(corruption).unwrap(), ReadPolicy::default());
    let parts = rescyo_part::read_partitions(&r).unwrap().partitions;
    let mut found: Vec<rescyo_ntfs::Found> = parts
        .iter()
        .flat_map(|p| probe_boot(&r, p.offset, Some(p.offset + p.size)).unwrap())
        .collect();
    let mut sink = VecSink::default();
    let cancel = CancelToken::new();
    let scan = deep_scan(&r, 0..r.geometry().size, &mut sink, &cancel).unwrap();
    found.extend(infer_volumes(&r, &scan));
    found.sort_by_key(|f| std::cmp::Reverse(f.score));
    let geo = found
        .first()
        .unwrap_or_else(|| panic!("{} {corruption}: no volume", s.expected.scenario))
        .geometry
        .clone();
    walk(&r, &geo, Some(&scan), &Limits::default(), &mut sink, &cancel).unwrap();
    (r, geo, sink)
}

fn ntfs() -> Vec<Scenario> {
    rescyo_testkit::scenarios()
        .into_iter()
        .filter(|s| s.expected.fs == "ntfs")
        .collect()
}

#[test]
fn intact_ntfs_recovers_every_file_with_path_and_mtime() {
    let _ = require_corpus!();
    for s in ntfs() {
        let e = &s.expected;
        let (r, _, sink) = recover(&s, "none");
        let by_path: HashMap<String, &rescyo_fs::Entry> = sink
            .entries
            .iter()
            .enumerate()
            .filter(|(_, x)| x.kind != EntryKind::Directory)
            .map(|(i, x)| (sink.path_of(EntryId(i as u64 + 1)), x))
            .collect();
        for f in e.files.iter().filter(|f| f.state == FileState::Live) {
            let got = by_path
                .get(&f.path)
                .unwrap_or_else(|| panic!("{}: missing {}", e.scenario, f.path));
            assert_eq!(got.size, f.size, "{}", f.path);
            assert_eq!(got.state, EntryState::Live, "{}", f.path);
            assert_eq!(sha(&read_layout(&r, &got.layout.extents)), f.sha256, "{}", f.path);
            assert_eq!(
                got.times.modified.map(|t| t.unix),
                Some(f.mtime),
                "{} mtime",
                f.path
            );
        }
    }
}

#[test]
fn damaged_ntfs_recovers_every_file_that_survives() {
    let _ = require_corpus!();
    let (mut deleted_total, mut deleted_ok) = (0u32, 0u32);
    let mut report = Vec::new();
    for s in ntfs() {
        let e = &s.expected;
        for corruption in &e.corruptions {
            let (r, geo, sink) = recover(&s, corruption);
            let shas: HashMap<String, u64> = sink
                .entries
                .iter()
                .filter(|x| x.kind != EntryKind::Directory)
                .map(|x| (sha(&read_layout(&r, &x.layout.extents)), x.size))
                .collect();
            let mut ok = 0;
            let live: Vec<_> = e.files.iter().filter(|f| f.state == FileState::Live).collect();
            for f in &live {
                assert!(
                    shas.get(&f.sha256) == Some(&f.size),
                    "{} {corruption} (mft at {:#x}): {} not recovered",
                    e.scenario,
                    geo.mft_offset,
                    f.path
                );
                ok += 1;
            }
            for f in e
                .files
                .iter()
                .filter(|f| f.state == FileState::Deleted && f.size > 0)
            {
                deleted_total += 1;
                if shas.contains_key(&f.sha256) {
                    deleted_ok += 1;
                }
            }
            report.push(format!(
                "{:<10} {:<15} {ok}/{} exact",
                e.scenario,
                corruption,
                live.len()
            ));
        }
    }
    eprintln!("{}\ndeleted: {deleted_ok}/{deleted_total}", report.join("\n"));
    assert!(
        deleted_ok * 100 >= deleted_total * 95,
        "deleted {deleted_ok}/{deleted_total}"
    );
}
