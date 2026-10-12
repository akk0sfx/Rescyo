//! Developer tasks for Rescyo. Run with `cargo xtask <task>`.
//!
//! - `corpus [scenario…]`  build the test corpus inside the corpus container (ADR-0002)
//! - `containers [scenario…]`  wrap the corpus images into VHD/VHDX/VMDK/qcow2/E01 with the
//!   reference tools (qemu-img, ewfacquire) inside their own container
//! - `corrupt <scenario> <corruption> <out.img>`  write a damaged copy for manual inspection
//! - `gate1`  Gate 1: every scenario and corruption through the shipping `rescyo` binary
//! - `fonts`  fetch the Noto Sans SC/JP/KR language packs (pinned, hash-checked) into
//!   `target/fonts` for the GUI and release packages
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const IMAGE_TAG: &str = "rescyo-corpus:dev";
/// google/fonts commit the CJK language packs come from (SIL OFL 1.1).
const FONTS_COMMIT: &str = "bd8f81ddb5c74d5c8897b36ad88b440266245103";
const CJK_FONTS: [(&str, &str, &str); 3] = [
    (
        "notosanssc",
        "NotoSansSC[wght].ttf",
        "a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da",
    ),
    (
        "notosansjp",
        "NotoSansJP[wght].ttf",
        "c2f3b4d463500a2ddcd3849cded1fceeb9fd6d1c32e6cbecd568453ba50fc68f",
    ),
    (
        "notosanskr",
        "NotoSansKR[wght].ttf",
        "194018e6b2b293a7964f037b25c0249ce1418bc9ab3c971060a03aa57861e252",
    ),
];
const CONTAINERS_TAG: &str = "rescyo-containers:dev";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("corpus") => corpus(args.get(1..).unwrap_or_default()),
        Some("containers") => containers(args.get(1..).unwrap_or_default()),
        Some("gate1") => gate1(),
        Some("fonts") => fonts(),
        Some("corrupt") => match args.get(1..) {
            Some([scenario, corruption, out]) => corrupt(scenario, corruption, Path::new(out)),
            _ => bail!("usage: cargo xtask corrupt <scenario> <corruption> <out.img>"),
        },
        _ => {
            eprintln!(
                "usage: cargo xtask <corpus [scenario…] | containers [scenario…] | corrupt <scenario> <corruption> <out.img> | gate1 | fonts>"
            );
            std::process::exit(2);
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Builds the corpus container and runs it. Every file-system tool runs inside the container's
/// own Linux VM; this process only invokes `docker` and reads/writes regular files.
fn corpus(only: &[String]) -> Result<()> {
    let root = workspace_root().canonicalize()?;
    let scenarios = root.join("corpus/scenarios");
    let out = rescyo_testkit::corpus_root();
    std::fs::create_dir_all(&out)?;
    let out = out.canonicalize()?;

    let status = Command::new("docker")
        .args(["build", "--quiet", "--tag", IMAGE_TAG])
        .arg(root.join("corpus/container"))
        .status()
        .context("running docker (is Docker running?)")?;
    if !status.success() {
        bail!("docker build failed");
    }
    // `--privileged` grants loop devices of the container VM, needed by exfat-fuse; no host
    // path other than the two directories below is visible inside.
    let status = Command::new("docker")
        .args(["run", "--rm", "--privileged", "--network", "none"])
        .arg("-v")
        .arg(format!("{}:/scenarios:ro", scenarios.display()))
        .arg("-v")
        .arg(format!("{}:/out", out.display()))
        .arg(IMAGE_TAG)
        .args(["/scenarios", "/out"])
        .args(only)
        .status()?;
    if !status.success() {
        bail!("corpus build failed");
    }
    for s in rescyo_testkit::scenarios() {
        println!(
            "{:<14} {:>4} files  corruptions: {}",
            s.expected.scenario,
            s.expected.files.len(),
            s.expected.corruptions.join(", ")
        );
    }
    Ok(())
}

/// Wraps the built corpus images into virtual-disk containers with qemu-img and ewfacquire,
/// inside a container that sees only the corpus output directory (ADR-0002).
fn containers(only: &[String]) -> Result<()> {
    let root = workspace_root().canonicalize()?;
    let out = rescyo_testkit::corpus_root();
    if !out.is_dir() {
        bail!("no corpus: run `cargo xtask corpus` first");
    }
    let out = out.canonicalize()?;
    let status = Command::new("docker")
        .args(["build", "--quiet", "--tag", CONTAINERS_TAG])
        .arg(root.join("corpus/containers"))
        .status()
        .context("running docker (is Docker running?)")?;
    if !status.success() {
        bail!("docker build failed");
    }
    let status = Command::new("docker")
        .args(["run", "--rm", "--network", "none"])
        .arg("-v")
        .arg(format!("{}:/out", out.display()))
        .arg(CONTAINERS_TAG)
        .args(only)
        .status()?;
    if !status.success() {
        bail!("wrapping the corpus into containers failed");
    }
    Ok(())
}

/// Writes `<scenario>` with `<corruption>` applied to a new file (never over the original).
fn corrupt(scenario: &str, corruption: &str, out: &Path) -> Result<()> {
    let s =
        rescyo_testkit::load(&rescyo_testkit::corpus_root().join(scenario)).map_err(anyhow::Error::msg)?;
    if out.exists() {
        bail!("{} already exists; refusing to overwrite", out.display());
    }
    let ranges = s.damage_ranges(corruption).map_err(anyhow::Error::msg)?;
    let mut data = std::fs::read(s.image_path())?;
    for r in ranges {
        let a = usize::try_from(r.start)?;
        let b = usize::try_from(r.end)?;
        data.get_mut(a..b).context("damage range outside image")?.fill(0);
    }
    std::fs::write(out, data)?;
    println!("wrote {}", out.display());
    Ok(())
}

/// Gate 1 (v0.1): with the real binary, scan each damaged image deeply and recover everything.
/// Every live file must come back byte for byte, except files whose directory entry the damage
/// destroyed and files Rescyo itself labelled a contiguous guess after every allocation table
/// was lost. At least 95% of deleted contiguous files must come back on the Tier A file systems
/// (ADR-0005); experimental ones must recover every live file, and their deleted files are only
/// reported.
fn gate1() -> Result<()> {
    let root = workspace_root().canonicalize()?;
    let status = Command::new("cargo")
        .args(["build", "--quiet", "--release", "-p", "rescyo-cli"])
        .current_dir(&root)
        .status()?;
    if !status.success() {
        bail!("build failed");
    }
    let bin = root
        .join("target/release")
        .join(if cfg!(windows) { "rescyo.exe" } else { "rescyo" });
    let scenarios = rescyo_testkit::scenarios();
    if scenarios.is_empty() {
        bail!("no corpus: run `cargo xtask corpus` first");
    }
    let work = std::env::temp_dir().join(format!("rescyo-gate1-{}", std::process::id()));
    let mut rows = Vec::new();
    let (mut failures, mut del_total, mut del_ok) = (0u32, 0u64, 0u64);
    for s in &scenarios {
        for corruption in &s.expected.corruptions {
            let dir = work.join(format!(
                "{}-{}",
                s.expected.scenario,
                corruption.replace('+', "_")
            ));
            std::fs::create_dir_all(&dir)?;
            let img = dir.join("image.img");
            corrupt_into(s, corruption, &img)?;
            let session = dir.join("s.db");
            let out = dir.join("out");
            let scan = Command::new(&bin)
                .args(["scan", "--deep", "--json"])
                .arg(&img)
                .arg("--session")
                .arg(&session)
                .output()?;
            if !scan.status.success() {
                bail!(
                    "{} {corruption}: scan failed: {}",
                    s.expected.scenario,
                    String::from_utf8_lossy(&scan.stdout)
                );
            }
            let rec = Command::new(&bin)
                .args(["recover", "--deleted", "--streams", "--json"])
                .arg(&session)
                .arg("--to")
                .arg(&out)
                .output()?;
            if !matches!(rec.status.code(), Some(0 | 5)) {
                bail!(
                    "{} {corruption}: recover failed: {}",
                    s.expected.scenario,
                    String::from_utf8_lossy(&rec.stdout)
                );
            }
            let report: serde_json::Value =
                serde_json::from_slice(&std::fs::read(out.join("_rescyo_report.json"))?)?;
            let files = report
                .get("files")
                .and_then(|f| f.as_array())
                .cloned()
                .unwrap_or_default();
            let shas: std::collections::HashSet<&str> =
                files.iter().filter_map(|f| f["sha256"].as_str()).collect();
            let guessed: std::collections::HashSet<&str> = files
                .iter()
                .filter(|f| f["assembly"] == "contiguous_assumed")
                .filter_map(|f| f["source_path"].as_str().and_then(|p| p.rsplit('/').next()))
                .collect();
            let integrity: std::collections::HashMap<&str, &str> = files
                .iter()
                .filter_map(|f| Some((f.get("sha256")?.as_str()?, f.get("integrity")?.as_str()?)))
                .collect();
            let root_lost = corruption.split('+').any(|c| c == "root");
            // Tier B (ADR-0005): ext4 and later HFS+, APFS, Btrfs.
            let experimental = s.expected.fs == "ext4";
            let mut verified = 0u32;
            let (mut live, mut exact, mut excused) = (0u32, 0u32, 0u32);
            let mut missing = Vec::new();
            for f in &s.expected.files {
                let name = f.path.rsplit('/').next().unwrap_or(&f.path);
                if f.state == rescyo_testkit::FileState::Deleted {
                    if f.size > 0 && f.fragmented != Some(true) && !experimental {
                        del_total += 1;
                        del_ok += u64::from(shas.contains(f.sha256.as_str()));
                    }
                    continue;
                }
                live += 1;
                if shas.contains(f.sha256.as_str()) {
                    exact += 1;
                    // A byte-exact copy of a verifiable format must also pass its content check.
                    let checkable = [".jpg", ".docx", ".xlsx"].iter().any(|x| f.path.ends_with(x));
                    match integrity.get(f.sha256.as_str()) {
                        Some(&"intact") => verified += 1,
                        Some(&"damaged") => missing.push(format!("{} (exact but reported damaged)", f.path)),
                        _ if checkable => missing.push(format!("{} (not verified)", f.path)),
                        _ => {}
                    }
                } else if (root_lost && f.in_root) || (f.fragmented != Some(false) && guessed.contains(name))
                {
                    excused += 1;
                } else {
                    missing.push(f.path.clone());
                }
            }
            let ok = missing.is_empty();
            failures += u32::from(!ok);
            rows.push(format!(
                "| {}{} | {} | {exact}/{live} | {verified} | {excused} | {} |",
                s.expected.scenario,
                if experimental { " (experimental)" } else { "" },
                corruption,
                if ok {
                    "pass".to_owned()
                } else {
                    format!("FAIL: {}", missing.join(", "))
                }
            ));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    let pct = if del_total == 0 {
        100.0
    } else {
        del_ok as f64 * 100.0 / del_total as f64
    };
    let deleted_ok = pct >= 95.0;
    let mut md = String::from(
        "## Gate 1\n\n| Scenario | Damage | Live exact | Verified intact | Excused | Result |\n|---|---|---|---|---|---|\n",
    );
    md.push_str(&rows.join("\n"));
    md.push_str(&format!(
        "\n\nDeleted contiguous files recovered: {del_ok}/{del_total} ({pct:.1}%, need 95%)\n\n**{}**\n",
        if failures == 0 && deleted_ok {
            "Gate 1 passed"
        } else {
            "Gate 1 FAILED"
        }
    ));
    println!("{md}");
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(summary)?;
        f.write_all(md.as_bytes())?;
    }
    if failures > 0 || !deleted_ok {
        bail!("Gate 1 failed");
    }
    Ok(())
}

fn corrupt_into(s: &rescyo_testkit::Scenario, corruption: &str, out: &Path) -> Result<()> {
    let ranges = s.damage_ranges(corruption).map_err(anyhow::Error::msg)?;
    let mut data = std::fs::read(s.image_path())?;
    for r in ranges {
        let a = usize::try_from(r.start)?;
        let b = usize::try_from(r.end)?;
        data.get_mut(a..b).context("damage range outside image")?.fill(0);
    }
    std::fs::write(out, data)?;
    Ok(())
}

/// Downloads the CJK language packs with curl (present on every CI runner and on Windows 10+)
/// and keeps them only if their SHA-256 matches the pinned value.
fn fonts() -> Result<()> {
    use sha2::{Digest, Sha256};
    let dir = workspace_root().join("target/fonts");
    std::fs::create_dir_all(&dir)?;
    for (family, file, want) in CJK_FONTS {
        let dest = dir.join(file);
        let ok = |p: &Path| -> bool {
            std::fs::read(p).is_ok_and(|b| {
                Sha256::digest(&b)
                    .iter()
                    .map(|x| format!("{x:02x}"))
                    .collect::<String>()
                    == want
            })
        };
        if ok(&dest) {
            println!("{file}: present");
            continue;
        }
        let url = format!(
            "https://raw.githubusercontent.com/google/fonts/{FONTS_COMMIT}/ofl/{family}/{}",
            file.replace('[', "%5B").replace(']', "%5D")
        );
        let tmp = dir.join(format!("{file}.part"));
        let status = Command::new("curl")
            .args(["-sSfL", "-o"])
            .arg(&tmp)
            .arg(&url)
            .status()?;
        if !status.success() {
            bail!("downloading {file} failed");
        }
        if !ok(&tmp) {
            let _ = std::fs::remove_file(&tmp);
            bail!("{file}: checksum mismatch, refusing it");
        }
        std::fs::rename(&tmp, &dest)?;
        println!("{file}: downloaded and verified");
    }
    let license = dir.join("NotoSans-OFL.txt");
    if !license.exists() {
        let url =
            format!("https://raw.githubusercontent.com/google/fonts/{FONTS_COMMIT}/ofl/notosanssc/OFL.txt");
        Command::new("curl")
            .args(["-sSfL", "-o"])
            .arg(&license)
            .arg(&url)
            .status()?;
    }
    Ok(())
}
