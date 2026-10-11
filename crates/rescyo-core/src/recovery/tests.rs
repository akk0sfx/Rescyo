use std::collections::BTreeSet;
use std::sync::Arc;

use rescyo_fs::{AssemblyMethod, DataLayout, Entry, ScanSink, Times, VolumeCandidate};
use rescyo_io::testing::MemSource;
use rescyo_io::{ReadPolicy, SourceFingerprint};

use super::*;
use crate::{ScanOptions, scan};

/// Puts the Source on disk A and anything else where the test says.
struct Fake {
    source: Placement,
    target: Placement,
    info: TargetInfo,
}

impl Fake {
    fn safe() -> Self {
        Self {
            source: Placement::Local(BTreeSet::from(["disk:A".to_owned()])),
            target: Placement::Local(BTreeSet::from(["disk:B".to_owned()])),
            info: TargetInfo {
                fs: Some("apfs".into()),
                free_bytes: Some(1 << 40),
            },
        }
    }
}

impl DeviceResolver for Fake {
    fn path(&self, _: &Path) -> Placement {
        self.target.clone()
    }
    fn source(&self, _: &SourceIdentity) -> Placement {
        self.source.clone()
    }
    fn target_info(&self, _: &Path) -> TargetInfo {
        self.info.clone()
    }
}

fn opts(target: &Path) -> RecoverOptions {
    RecoverOptions {
        target: target.to_path_buf(),
        selection: Selection::default(),
        allow_unverified_target: false,
        verify: true,
    }
}

fn sha_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn recovers_a_scenario_byte_for_byte_with_times_and_report_then_resumes() {
    let Some(s) = rescyo_testkit::scenarios()
        .into_iter()
        .find(|s| s.expected.scenario == "fat32-mbr")
    else {
        eprintln!("skipped: corpus not generated");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let r = GuardedReader::new(s.open("none").unwrap(), ReadPolicy::default());
    let fp = SourceFingerprint::compute(&r).unwrap();
    let mut session = Session::create(&dir.path().join("s.db"), r.identity(), &fp).unwrap();
    scan(
        &r,
        &mut session,
        &ScanOptions::default(),
        &mut |_| {},
        &CancelToken::new(),
    )
    .unwrap();

    let target = dir.path().join("out");
    let p = plan(&session, &Fake::safe(), &opts(&target)).unwrap();
    assert!(p.problems.is_empty(), "{:?}", p.problems);
    let rep = run(&r, &session, &p, &mut |_| {}, &CancelToken::new()).unwrap();
    assert!(rep.is_clean(), "{:?}", rep.totals);
    for f in s
        .expected
        .files
        .iter()
        .filter(|f| f.state == rescyo_testkit::FileState::Live)
    {
        let path = f.path.split('/').fold(target.clone(), |p, c| p.join(c));
        let data = std::fs::read(&path).unwrap_or_else(|_| panic!("missing {}", f.path));
        assert_eq!(sha_hex(&data), f.sha256, "{}", f.path);
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        // FAT times are local; the corpus was written in UTC, so compare through the same rule.
        let expect = system_time(&Timestamp {
            unix: f.mtime,
            nanos: 0,
            zone: TimeZone::Local,
        })
        .unwrap();
        assert_eq!(mtime, expect, "{} mtime", f.path);
    }
    let json = report::RecoveryReport::read_json(&target.join(report::JSON_NAME)).unwrap();
    assert_eq!(json.totals.files, rep.totals.files);
    assert!(json.files.iter().all(|f| f.sha256.is_some()));
    assert!(
        std::fs::read_to_string(target.join(report::CSV_NAME))
            .unwrap()
            .contains("fat_chain")
    );

    // A second run into the same Target verifies and skips everything; nothing is overwritten.
    let rep2 = run(
        &r,
        &session,
        &plan(&session, &Fake::safe(), &opts(&target)).unwrap(),
        &mut |_| {},
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(rep2.totals.resumed, rep.totals.files);
}

#[test]
fn refuses_the_source_device_and_unknown_targets() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = tiny_session(dir.path(), 10);
    let mut same = Fake::safe();
    same.target = Placement::Local(BTreeSet::from(["disk:A".to_owned(), "disk:C".to_owned()]));
    let p = plan(&session, &same, &opts(&dir.path().join("t"))).unwrap();
    assert_eq!(
        p.problems,
        vec![Problem::TargetOnSourceDevice {
            devices: vec!["disk:A".into()]
        }]
    );

    let mut unknown = Fake::safe();
    unknown.target = Placement::Unknown("fuse".into());
    let p = plan(&session, &unknown, &opts(&dir.path().join("t"))).unwrap();
    assert!(matches!(
        p.problems.as_slice(),
        [Problem::UnverifiedTarget { .. }]
    ));
    let mut o = opts(&dir.path().join("t"));
    o.allow_unverified_target = true;
    let p = plan(&session, &unknown, &o).unwrap();
    assert!(p.problems.is_empty() && p.target_unverified);

    let mut remote = Fake::safe();
    remote.target = Placement::Remote("smbfs".into());
    assert!(
        plan(&session, &remote, &opts(&dir.path().join("t")))
            .unwrap()
            .problems
            .is_empty()
    );
}

#[test]
fn preflight_reports_space_and_fat32_limits_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let (session, r) = tiny_session(dir.path(), 5 << 30);
    let mut fake = Fake::safe();
    fake.info = TargetInfo {
        fs: Some("fat32".into()),
        free_bytes: Some(1 << 30),
    };
    let target = dir.path().join("t");
    let p = plan(&session, &fake, &opts(&target)).unwrap();
    assert!(
        p.problems
            .iter()
            .any(|x| matches!(x, Problem::FileTooLarge { size, .. } if *size == 5 << 30))
    );
    assert!(
        p.problems
            .iter()
            .any(|x| matches!(x, Problem::InsufficientSpace { .. }))
    );
    assert!(matches!(
        run(&r, &session, &p, &mut |_| {}, &CancelToken::new()),
        Err(RecoveryError::Blocked(_))
    ));
    assert!(
        !target.exists(),
        "nothing may be written when the preflight fails"
    );
}

#[test]
fn windows_rules_rename_and_report_it() {
    let dir = tempfile::tempdir().unwrap();
    let (session, r) = tiny_session(dir.path(), 3);
    let mut fake = Fake::safe();
    fake.info.fs = Some("exfat".into());
    let target = dir.path().join("t");
    let p = plan(&session, &fake, &opts(&target)).unwrap();
    assert_eq!(p.items[0].target, vec!["a_b_.txt".to_owned()]);
    assert!(p.items[0].renamed);
    let rep = run(&r, &session, &p, &mut |_| {}, &CancelToken::new()).unwrap();
    assert_eq!(std::fs::read(target.join("a_b_.txt")).unwrap(), b"abc");
    assert!(rep.files[0].renamed);
}

/// A Session over an in-memory Source with one file `a:b?.txt` of `size` bytes ("abc…").
fn tiny_session(dir: &Path, size: u64) -> (Session, GuardedReader) {
    let mut data = vec![0u8; 1 << 16];
    data[4096..4099].copy_from_slice(b"abc");
    let r = GuardedReader::new(Arc::new(MemSource::new(data, 512)), ReadPolicy::default());
    let fp = SourceFingerprint::compute(&r).unwrap();
    let mut s = Session::create(&dir.join("s.db"), r.identity(), &fp).unwrap();
    let cand = VolumeCandidate {
        fs: rescyo_fs::FsKind::Fat32,
        offset: Some(0),
        cluster_size: 512,
        data_offset: 0,
        size: None,
        label: None,
        evidence: vec![],
        score: 1000,
        params: serde_json::Value::Null,
    };
    let v = s.add_volume(&cand).unwrap();
    s.mark_walked(v).unwrap();
    let mut sink = s.sink(v).unwrap();
    let mut layout = DataLayout::default();
    layout.push_disk(4096, size.min(3));
    sink.entry(Entry {
        parent: Parent::Root,
        name: "a:b?.txt".into(),
        name_raw: None,
        kind: EntryKind::File,
        state: EntryState::Live,
        size,
        times: Times::default(),
        layout,
        assembly: AssemblyMethod::FatChain,
        fs_ref: 9,
        seed: None,
    })
    .unwrap();
    sink.finish().unwrap();
    (s, r)
}
