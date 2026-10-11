//! End-to-end runs of the `rescyo` binary on corpus images (image sources only: no disk of the
//! machine running the tests is ever opened, ADR-0002).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration test"
)]

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn rescyo(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rescyo"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run rescyo")
}

fn json(o: &Output) -> Value {
    let text = String::from_utf8_lossy(&o.stdout);
    let last = text
        .lines()
        .last()
        .unwrap_or_else(|| panic!("no output; stderr: {}", String::from_utf8_lossy(&o.stderr)));
    serde_json::from_str(last).unwrap_or_else(|e| panic!("{e}: {last}"))
}

fn image(name: &str) -> Option<std::path::PathBuf> {
    let s = rescyo_testkit::scenarios()
        .into_iter()
        .find(|s| s.expected.scenario == name)?;
    Some(s.image_path())
}

#[test]
fn scan_ls_recover_info_round_trip() {
    let Some(img) = image("fat16-mbr") else {
        eprintln!("skipped: corpus not generated");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("flash.db");
    let (img, session_s) = (img.to_str().unwrap(), session.to_str().unwrap());

    let o = rescyo(&["scan", img, "--session", session_s, "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let v = json(&o);
    assert_eq!(
        (
            v["schema_version"].as_u64(),
            v["command"].as_str(),
            v["ok"].as_bool()
        ),
        (Some(1), Some("scan"), Some(true))
    );
    assert_eq!(v["data"]["partition_table"], "mbr");
    assert_eq!(v["data"]["volumes"][0]["fs"], "fat16");

    // A second scan never overwrites the session.
    let o = rescyo(&["scan", img, "--session", session_s, "--json"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(json(&o)["error"]["code"], "session_exists");

    let o = rescyo(&["ls", session_s, "--ext", "jpg", "--json"]);
    let v = json(&o);
    let paths: Vec<&str> = v["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"写真/東京タワー.jpg"), "{paths:?}");
    assert!(paths.iter().all(|p| p.ends_with(".jpg")));

    let o = rescyo(&["ls", session_s, "--only-deleted", "--json"]);
    assert!(
        json(&o)["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["state"] == "deleted")
    );

    let out = dir.path().join("out");
    let o = rescyo(&["recover", session_s, "--to", out.to_str().unwrap(), "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stdout));
    let v = json(&o);
    assert!(v["data"]["totals"]["files"].as_u64().unwrap() > 300);
    assert!(Path::new(v["data"]["report_json"].as_str().unwrap()).exists());
    assert!(out.join("Документы/Бухгалтерия/Баланс.xlsx").exists());

    // Nothing selected: the preflight stops before writing, exit code 4.
    let o = rescyo(&[
        "recover",
        session_s,
        "--to",
        dir.path().join("none").to_str().unwrap(),
        "--ext",
        "nope",
        "--json",
    ]);
    assert_eq!(o.status.code(), Some(4));
    assert_eq!(json(&o)["error"]["code"], "preflight_failed");
    assert!(!dir.path().join("none").exists());

    let o = rescyo(&["info", session_s, "--json"]);
    let v = json(&o);
    assert!(
        v["data"]["journal"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["action"] == "recovery_finished")
    );

    let o = rescyo(&["volumes", session_s, "--json"]);
    assert_eq!(json(&o)["data"]["volumes"][0]["walked"], true);
}

#[test]
fn human_output_is_localized_and_errors_have_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.img");
    let o = rescyo(&[
        "scan",
        missing.to_str().unwrap(),
        "--session",
        dir.path().join("s.db").to_str().unwrap(),
        "--lang",
        "ru",
    ]);
    assert_eq!(o.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("Не удалось открыть"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = rescyo(&[
        "scan",
        missing.to_str().unwrap(),
        "--session",
        dir.path().join("s.db").to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(json(&o)["error"]["code"], "source_open_failed");
    let o = rescyo(&["frobnicate"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn json_stream_emits_progress_then_the_envelope() {
    let Some(img) = image("fat12-floppy") else { return };
    let dir = tempfile::tempdir().unwrap();
    let o = rescyo(&[
        "scan",
        img.to_str().unwrap(),
        "--session",
        dir.path().join("s.db").to_str().unwrap(),
        "--deep",
        "--json-stream",
    ]);
    let text = String::from_utf8_lossy(&o.stdout);
    let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(lines.iter().any(|l| l["type"] == "progress"));
    assert_eq!(lines.last().unwrap()["ok"], true);
}

#[test]
fn image_copies_with_a_ddrescue_map_and_scan_honours_it() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("disk.bin");
    let data: Vec<u8> = (0..1u32 << 20)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13).to_le_bytes()[0])
        .collect();
    std::fs::write(&src, &data).unwrap();
    let out = dir.path().join("disk.img");
    let (src_s, out_s) = (src.to_str().unwrap(), out.to_str().unwrap());

    let o = rescyo(&["image", src_s, "--to", out_s, "--block-kib", "64", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let v = json(&o);
    assert_eq!(v["command"], "image");
    assert_eq!(v["data"]["rescued"], 1u64 << 20);
    assert_eq!(v["data"]["bad_areas"], 0);
    assert_eq!(std::fs::read(&out).unwrap(), data);
    let map = std::fs::read_to_string(dir.path().join("disk.img.map")).unwrap();
    assert!(map.contains("0x00000000  0x00100000  +"), "{map}");

    // An image without a map is never overwritten.
    std::fs::remove_file(dir.path().join("disk.img.map")).unwrap();
    let o = rescyo(&["image", src_s, "--to", out_s, "--json"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(json(&o)["error"]["code"], "image_exists");

    // A map with unread areas makes a scan report them as unreadable sectors.
    let bad_map = dir.path().join("bad.map");
    std::fs::write(
        &bad_map,
        "# Mapfile. Created by GNU ddrescue\n0x00000000 + 1\n#      pos        size  status\n\
         0x00000000  0x0000C800  +\n0x0000C800  0x00001000  -\n0x0000D800  0x000F2800  +\n",
    )
    .unwrap();
    let session = dir.path().join("s.db");
    let o = rescyo(&[
        "scan",
        out_s,
        "--session",
        session.to_str().unwrap(),
        "--deep",
        "--map",
        bad_map.to_str().unwrap(),
        "--continue-on-errors",
        "--json",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(json(&o)["data"]["unreadable_sectors"], 8);

    // A map for another disk size is refused.
    std::fs::write(&bad_map, "0x00000000 + 1\n0x00000000  0x00000200  +\n").unwrap();
    let o = rescyo(&[
        "scan",
        out_s,
        "--session",
        dir.path().join("t.db").to_str().unwrap(),
        "--map",
        bad_map.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(o.status.code(), Some(3));
    assert_eq!(json(&o)["error"]["code"], "map_size_mismatch");
}

#[test]
fn partitions_finds_a_partition_whose_table_was_wiped() {
    let Some(img) = image("fat32-mbr") else {
        eprintln!("skipped: corpus not generated");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("wiped.img");
    let mut data = std::fs::read(img).unwrap();
    data[..512].fill(0);
    std::fs::write(&copy, &data).unwrap();
    let o = rescyo(&["partitions", copy.to_str().unwrap(), "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let v = json(&o);
    assert_eq!(v["data"]["partition_table"], "none");
    let parts = v["data"]["partitions"].as_array().unwrap();
    assert_eq!(parts.len(), 1, "{parts:?}");
    assert_eq!(parts[0]["offset"], 1 << 20);
    assert_eq!(parts[0]["fs"], "fat32");
    assert_eq!(parts[0]["status"], "lost");
    assert_eq!(parts[0]["backup_boot"], true);

    // People see it named as lost, in their language.
    let o = rescyo(&["partitions", copy.to_str().unwrap(), "--lang", "ru"]);
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("Потерянный раздел"), "{text}");
}

#[test]
fn containers_scan_like_their_raw_image_and_image_converts_them() {
    let Some(img) = image("ntfs-mbr") else { return };
    let containers = img.parent().unwrap().join("containers");
    if !containers.join("containers.json").exists() {
        eprintln!("skipped: no containers (run `cargo xtask containers`)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let listing = |source: &Path, tag: &str| -> Vec<String> {
        let session = dir.path().join(format!("{tag}.db"));
        let o = rescyo(&[
            "scan",
            source.to_str().unwrap(),
            "--session",
            session.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(
            o.status.code(),
            Some(0),
            "{tag}: {}",
            String::from_utf8_lossy(&o.stdout)
        );
        let o = rescyo(&["ls", session.to_str().unwrap(), "--deleted", "--json"]);
        let mut paths: Vec<String> = json(&o)["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| format!("{} {}", e["path"], e["size"]))
            .collect();
        paths.sort();
        paths
    };
    let want = listing(&img, "raw");
    assert!(want.len() > 300);
    for (file, tag) in [
        ("dynamic.vhdx", "vhdx"),
        ("stream.vmdk", "vmdk"),
        ("evidence.E02", "e01"),
    ] {
        assert_eq!(listing(&containers.join(file), tag), want, "{file}");
    }

    // People see which container was opened; `image` turns it back into the raw image.
    let o = rescyo(&[
        "scan",
        containers.join("compressed.qcow2").to_str().unwrap(),
        "--session",
        dir.path().join("q.db").to_str().unwrap(),
        "--lang",
        "en",
    ]);
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("QCOW2 image"), "{text}");
    let raw = dir.path().join("back.img");
    let o = rescyo(&[
        "image",
        containers.join("dynamic.vhd").to_str().unwrap(),
        "--to",
        raw.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stdout));
    assert_eq!(std::fs::read(&raw).unwrap(), std::fs::read(&img).unwrap());
}

#[test]
fn unsupported_and_orphaned_images_fail_with_their_own_codes() {
    let dir = tempfile::tempdir().unwrap();
    let scan = |p: &Path, session: &str| {
        rescyo(&[
            "scan",
            p.to_str().unwrap(),
            "--session",
            dir.path().join(session).to_str().unwrap(),
            "--json",
        ])
    };
    // An encrypted qcow2 header.
    let mut q = vec![0u8; 512];
    q[0..4].copy_from_slice(b"QFI\xfb");
    q[4..8].copy_from_slice(&2u32.to_be_bytes());
    q[20..24].copy_from_slice(&16u32.to_be_bytes());
    q[24..32].copy_from_slice(&(1u64 << 20).to_be_bytes());
    q[32..36].copy_from_slice(&1u32.to_be_bytes());
    let enc = dir.path().join("enc.qcow2");
    std::fs::write(&enc, &q).unwrap();
    let o = scan(&enc, "a.db");
    assert_eq!(o.status.code(), Some(3));
    assert_eq!(json(&o)["error"]["code"], "image_unsupported");

    // A qcow2 overlay whose backing file is gone.
    q[32..36].fill(0);
    let name = b"gone.qcow2";
    q[8..16].copy_from_slice(&256u64.to_be_bytes());
    q[16..20].copy_from_slice(&u32::try_from(name.len()).unwrap().to_be_bytes());
    q[256..256 + name.len()].copy_from_slice(name);
    let top = dir.path().join("top.qcow2");
    std::fs::write(&top, &q).unwrap();
    let o = scan(&top, "b.db");
    assert_eq!(o.status.code(), Some(3));
    assert_eq!(json(&o)["error"]["code"], "parent_image_missing");
}
