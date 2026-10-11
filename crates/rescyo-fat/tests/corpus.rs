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

/// What a user's deep scan would do: probe every partition start and offset 0, scan the whole
/// Source, merge candidates, take the best one and walk it with the scan's seeds.
fn recover_damaged(s: &Scenario, corruption: &str) -> (GuardedReader, Option<FatGeometry>, VecSink) {
    use rescyo_fat::{deep_scan, infer_volumes, seeds_for};
    let r = GuardedReader::new(s.open(corruption).unwrap(), ReadPolicy::default());
    let mut starts = vec![0u64];
    starts.extend(
        rescyo_part::read_partitions(&r)
            .unwrap()
            .partitions
            .iter()
            .map(|p| p.offset),
    );
    let mut sink = VecSink::default();
    let cancel = CancelToken::new();
    let scan = deep_scan(&r, 0..r.geometry().size, &mut sink, &cancel).unwrap();
    let mut found: Vec<rescyo_fat::Found> = starts.iter().flat_map(|&o| probe_boot(&r, o).unwrap()).collect();
    found.extend(infer_volumes(&r, &scan).unwrap());
    found.sort_by_key(|f| std::cmp::Reverse(f.score));
    let Some(best) = found.first() else {
        return (r, None, sink);
    };
    let geo = best.geometry.clone();
    let opts = WalkOptions {
        seeds: seeds_for(&geo, &scan),
        ..WalkOptions::default()
    };
    walk(&r, &geo, &opts, &Limits::default(), &mut sink, &cancel).unwrap();
    (r, Some(geo), sink)
}

#[test]
fn damaged_volumes_recover_every_file_that_survives() {
    let _ = require_corpus!();
    let (mut deleted_total, mut deleted_ok) = (0u32, 0u32);
    let mut report = Vec::new();
    for s in fat_scenarios() {
        let e = &s.expected;
        for corruption in e.corruptions.iter().filter(|c| *c != "none") {
            let (r, geo, sink) = recover_damaged(&s, corruption);
            let geo = geo.unwrap_or_else(|| panic!("{} {corruption}: no volume found", e.scenario));
            let root_lost = corruption.split('+').any(|c| c == "root");
            let mut by_sha: HashMap<String, Vec<&rescyo_fs::Entry>> = HashMap::new();
            let mut by_name: HashMap<String, Vec<&rescyo_fs::Entry>> = HashMap::new();
            for x in sink.entries.iter().filter(|x| x.kind == EntryKind::File) {
                by_sha
                    .entry(sha(&read_layout(&r, &x.layout.extents)))
                    .or_default()
                    .push(x);
                by_name.entry(x.name.clone()).or_default().push(x);
            }
            let (mut live, mut live_ok, mut excused) = (0, 0, 0);
            for f in &e.files {
                let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_owned();
                if f.state == FileState::Deleted {
                    if f.fragmented != Some(true) && f.size > 0 {
                        deleted_total += 1;
                        if by_sha.contains_key(&f.sha256) {
                            deleted_ok += 1;
                        }
                    }
                    continue;
                }
                live += 1;
                if by_sha
                    .get(&f.sha256)
                    .is_some_and(|v| v.iter().any(|x| x.size == f.size))
                {
                    live_ok += 1;
                    continue;
                }
                // Honest exceptions: the entry itself was destroyed, or Rescyo labelled the file
                // a contiguous guess because no allocation table survived.
                let entry_destroyed = root_lost && f.in_root;
                let labelled_guess = f.fragmented != Some(false)
                    && by_name
                        .get(&name)
                        .is_some_and(|v| v.iter().any(|x| x.assembly.is_guess()));
                assert!(
                    entry_destroyed || labelled_guess,
                    "{} {corruption} ({:?}, cluster {}, data {:#x}): live {} not recovered",
                    e.scenario,
                    geo.fs,
                    geo.cluster_size,
                    geo.data_offset,
                    f.path
                );
                excused += 1;
            }
            report.push(format!(
                "{:<13} {:<20} {live_ok}/{live} exact, {excused} excused",
                e.scenario, corruption
            ));
        }
    }
    eprintln!("{}", report.join("\n"));
    eprintln!("deleted contiguous files: {deleted_ok}/{deleted_total}");
    assert!(
        deleted_ok * 100 >= deleted_total * 95,
        "deleted recovery {deleted_ok}/{deleted_total} below 95%"
    );
}
