//! Walks every FAT and exFAT scenario of the test corpus and checks each file byte for byte.
#![allow(
    clippy::cast_possible_truncation,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration test helpers"
)]

use std::collections::HashMap;

use rescyo_fat::{FatGeometry, WalkOptions, probe_boot, walk};
use rescyo_fs::{CancelToken, EntryId, EntryKind, EntryState, Extent, Limits, VecSink};
use rescyo_io::{GuardedReader, ReadPolicy};
use rescyo_testkit::{FileState, Scenario, require_corpus};
use sha2::{Digest, Sha256};

pub fn volume_offset(r: &GuardedReader) -> u64 {
    rescyo_part::read_partitions(r)
        .unwrap()
        .partitions
        .first()
        .map_or(0, |p| p.offset)
}

pub fn read_layout(r: &GuardedReader, extents: &[Extent]) -> Vec<u8> {
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

fn fat_scenarios() -> Vec<Scenario> {
    rescyo_testkit::scenarios()
        .into_iter()
        .filter(|s| s.expected.fs.starts_with("fat") || s.expected.fs == "exfat")
        .collect()
}

fn walk_scenario(s: &Scenario, corruption: &str) -> (GuardedReader, FatGeometry, VecSink) {
    let r = GuardedReader::new(s.open(corruption).unwrap(), ReadPolicy::default());
    let off = volume_offset(&r);
    let found = probe_boot(&r, off).unwrap();
    let geo = found
        .first()
        .unwrap_or_else(|| panic!("{}: no boot sector at {off}", s.expected.scenario))
        .geometry
        .clone();
    let mut sink = VecSink::default();
    walk(
        &r,
        &geo,
        &WalkOptions::default(),
        &Limits::default(),
        &mut sink,
        &CancelToken::new(),
    )
    .unwrap();
    (r, geo, sink)
}

#[test]
fn intact_volumes_recover_every_file_byte_for_byte() {
    let _ = require_corpus!();
    for s in fat_scenarios() {
        let e = &s.expected;
        let (r, geo, sink) = walk_scenario(&s, "none");
        assert_eq!(geo.fs.as_str(), e.fs, "{}", e.scenario);
        let by_path: HashMap<String, (EntryId, &rescyo_fs::Entry)> = sink
            .entries
            .iter()
            .enumerate()
            .filter(|(_, x)| x.kind == EntryKind::File)
            .map(|(i, x)| {
                let id = EntryId(i as u64 + 1);
                (sink.path_of(id), (id, x))
            })
            .collect();
        let deleted_shas: Vec<String> = sink
            .entries
            .iter()
            .filter(|x| x.kind == EntryKind::File && x.state == EntryState::Deleted)
            .map(|x| sha(&read_layout(&r, &x.layout.extents)))
            .collect();
        for f in &e.files {
            match f.state {
                FileState::Live => {
                    let (_, got) = by_path
                        .get(&f.path)
                        .unwrap_or_else(|| panic!("{}: missing {}", e.scenario, f.path));
                    assert_eq!(got.size, f.size, "{}: {}", e.scenario, f.path);
                    assert_eq!(got.state, EntryState::Live, "{}: {}", e.scenario, f.path);
                    let data = read_layout(&r, &got.layout.extents);
                    assert_eq!(
                        sha(&data),
                        f.sha256,
                        "{}: {} ({:?})",
                        e.scenario,
                        f.path,
                        got.assembly
                    );
                    assert!(
                        !got.assembly.is_guess(),
                        "{}: {} assembled by a guess",
                        e.scenario,
                        f.path
                    );
                    let m = got
                        .times
                        .modified
                        .unwrap_or_else(|| panic!("{}: {} has no mtime", e.scenario, f.path));
                    assert_eq!(m.unix, f.mtime, "{}: {} mtime", e.scenario, f.path);
                }
                FileState::Deleted => {
                    if f.fragmented != Some(true) && f.size > 0 {
                        assert!(
                            deleted_shas.contains(&f.sha256),
                            "{}: deleted {} not recovered",
                            e.scenario,
                            f.path
                        );
                    }
                }
            }
        }
    }
}
