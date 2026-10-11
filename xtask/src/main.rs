//! Developer tasks for Rescyo. Run with `cargo xtask <task>`.
//!
//! - `corpus [scenario…]`  build the test corpus inside the corpus container (ADR-0002)
//! - `corrupt <scenario> <corruption> <out.img>`  write a damaged copy for manual inspection
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const IMAGE_TAG: &str = "rescyo-corpus:dev";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("corpus") => corpus(args.get(1..).unwrap_or_default()),
        Some("corrupt") => match args.get(1..) {
            Some([scenario, corruption, out]) => corrupt(scenario, corruption, Path::new(out)),
            _ => bail!("usage: cargo xtask corrupt <scenario> <corruption> <out.img>"),
        },
        _ => {
            eprintln!("usage: cargo xtask <corpus [scenario…] | corrupt <scenario> <corruption> <out.img>>");
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
