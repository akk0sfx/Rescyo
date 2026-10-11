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
