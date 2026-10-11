//! DMDE import without a deep scan: the log's grid and folder list must be enough.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "integration test"
)]

use std::collections::HashSet;

use rescyo_core::{ImportError, ImportOptions, Session, import_dmde};
use rescyo_fs::{CancelToken, EntryKind, Extent, VecSink};
use rescyo_io::{GuardedReader, ReadPolicy, SourceFingerprint};
use rescyo_testkit::FileState;
use sha2::{Digest, Sha256};

fn sha(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// A log in DMDE's format describing `geo`, with the directory starts our own scan found.
fn fake_log(r: &GuardedReader, cluster: u32, data: u64) -> String {
    let mut sink = VecSink::default();
    let scan = rescyo_fat::deep_scan(r, 0..r.geometry().size, &mut sink, &CancelToken::new()).unwrap();
    let dirs: Vec<String> = scan
        .dir_starts
        .iter()
        .map(|d| format!("{:#x}:{}:{}", d.offset, d.me, d.parent))
        .collect();
    format!(
        "[devscan]\r\nscan_dev=1:0:11:-1:0x0:512:{}:\"\\\\?\\usbstor#test\"\r\n[fatscan]\r\nvolumes_number=1\r\nvolume0=\"FS:2; CLUS:{cluster:#x}; start:{data:#x}; size:0x0\"\r\nvolume0_params=\"-1; 0; 0; 0; {}; 0x7; 0\"\r\ndirclus=\"{};{},\"\r\n",
        r.geometry().size / 512,
        dirs.len(),
        dirs.len(),
        dirs.join(",\\\r\n")
    )
}

#[test]
fn import_finds_a_volume_with_no_boot_sector_and_no_partition_table() {
    let Some(s) = rescyo_testkit::scenarios()
        .into_iter()
        .find(|s| s.expected.scenario == "fat32-mbr")
    else {
        eprintln!("skipped: corpus not generated");
        return;
    };
    let lay = &s.expected.layout;
    let (cluster, data) = (
        lay["cluster"].as_u64().unwrap() as u32,
        lay["data_offset"].as_u64().unwrap(),
    );
    let r = GuardedReader::new(s.open("mbr+boot_and_backup").unwrap(), ReadPolicy::default());
    let log = rescyo_dmde::parse(fake_log(&r, cluster, data).as_bytes()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fp = SourceFingerprint::compute(&r).unwrap();
    let mut session = Session::create(&dir.path().join("s.db"), r.identity(), &fp).unwrap();
    let (summary, report) = import_dmde(
        &r,
        &mut session,
        &log,
        &ImportOptions::default(),
        &mut |_| {},
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(summary.off_grid, 0);
    assert!(
        report.volumes.iter().any(|v| v.walked && v.data_offset == data),
        "{report:?}"
    );
    let files = session.entries(|e| e.entry.kind == EntryKind::File).unwrap();
    let shas: HashSet<String> = files
        .iter()
        .map(|e| {
            let mut out = Vec::new();
            for x in &e.entry.layout.extents {
                match x {
                    Extent::Disk { offset, len } => {
                        let mut b = vec![0u8; *len as usize];
                        r.read(*offset, &mut b).unwrap();
                        out.extend_from_slice(&b);
                    }
                    Extent::Zero { len } => out.resize(out.len() + *len as usize, 0),
                    Extent::Inline { data } => out.extend_from_slice(data),
                }
            }
            sha(&out)
        })
        .collect();
    for f in s.expected.files.iter().filter(|f| f.state == FileState::Live) {
        assert!(shas.contains(&f.sha256), "{} not recovered", f.path);
    }
}

#[test]
fn import_refuses_a_log_from_another_device() {
    let Some(s) = rescyo_testkit::scenarios()
        .into_iter()
        .find(|s| s.expected.scenario == "fat16-mbr")
    else {
        return;
    };
    let r = GuardedReader::new(s.open("none").unwrap(), ReadPolicy::default());
    let log = rescyo_dmde::parse(include_bytes!(
        "../../rescyo-dmde/tests/fixtures/devscan-flash.log"
    ))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fp = SourceFingerprint::compute(&r).unwrap();
    let mut session = Session::create(&dir.path().join("s.db"), r.identity(), &fp).unwrap();
    let err = import_dmde(
        &r,
        &mut session,
        &log,
        &ImportOptions::default(),
        &mut |_| {},
        &CancelToken::new(),
    );
    assert!(matches!(err, Err(ImportError::GeometryMismatch { .. })));
}
