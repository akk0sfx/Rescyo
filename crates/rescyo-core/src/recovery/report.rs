//! The Report: one record per recovered Entry, written as JSON and CSV into the Target.

use std::io::Write;
use std::path::Path;

use rescyo_fs::{AssemblyMethod, EntryState};
use serde::{Deserialize, Serialize};

pub const REPORT_SCHEMA_VERSION: u32 = 1;
pub const JSON_NAME: &str = "_rescyo_report.json";
pub const CSV_NAME: &str = "_rescyo_report.csv";

/// What content verification concluded (see Integrity in CONTEXT.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Integrity {
    Intact,
    Partial,
    Damaged,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    pub source_path: String,
    pub target_path: String,
    pub size: u64,
    pub state: EntryState,
    pub assembly: AssemblyMethod,
    pub integrity: Integrity,
    /// Why the content check failed, when `integrity` is `damaged`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity_detail: Option<String>,
    pub sha256: Option<String>,
    /// Byte ranges of the file `[start, length]` that were unreadable and written as zeros.
    pub unreadable: Vec<[u64; 2]>,
    /// The Target name differs from the source name (sanitized or de-duplicated).
    pub renamed: bool,
    /// Already present from an earlier run and verified by hash.
    pub resumed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    pub files: u64,
    pub bytes: u64,
    pub intact: u64,
    pub partial: u64,
    pub damaged: u64,
    pub unverified: u64,
    pub guessed: u64,
    pub resumed: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub path: String,
    /// The Session was force-rebound: the Source is not proven to be the scanned one.
    pub unverified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub schema_version: u32,
    pub tool_version: String,
    pub session: String,
    pub source: SourceInfo,
    pub target: String,
    /// The Target's devices were not verified (explicit override).
    pub target_unverified: bool,
    pub started: i64,
    pub finished: i64,
    pub totals: Totals,
    pub files: Vec<FileRecord>,
}

impl RecoveryReport {
    pub fn tally(&mut self) {
        let mut t = Totals::default();
        for f in &self.files {
            t.files += 1;
            if f.error.is_some() {
                t.failed += 1;
                continue;
            }
            t.bytes += f.size;
            match f.integrity {
                Integrity::Intact => t.intact += 1,
                Integrity::Partial => t.partial += 1,
                Integrity::Damaged => t.damaged += 1,
                Integrity::Unverified => t.unverified += 1,
            }
            t.guessed += u64::from(f.assembly.is_guess());
            t.resumed += u64::from(f.resumed);
        }
        self.totals = t;
    }

    /// Fully successful: nothing failed, partial or damaged.
    pub fn is_clean(&self) -> bool {
        self.totals.failed == 0 && self.totals.partial == 0 && self.totals.damaged == 0
    }

    pub fn write_json(&self, path: &Path) -> std::io::Result<()> {
        let mut f = std::fs::File::create(path)?;
        serde_json::to_writer_pretty(&mut f, self)?;
        f.write_all(b"\n")
    }

    pub fn write_csv(&self, path: &Path) -> std::io::Result<()> {
        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        // UTF-8 BOM so spreadsheet programs on Windows read non-ASCII names correctly.
        f.write_all(b"\xEF\xBB\xBF")?;
        writeln!(
            f,
            "source_path,target_path,size,state,assembly,integrity,sha256,unreadable_bytes,renamed,resumed,error"
        )?;
        for r in &self.files {
            let unreadable: u64 = r.unreadable.iter().map(|u| u[1]).sum();
            writeln!(
                f,
                "{},{},{},{},{},{},{},{},{},{},{}",
                csv(&r.source_path),
                csv(&r.target_path),
                r.size,
                match r.state {
                    EntryState::Live => "live",
                    EntryState::Deleted => "deleted",
                },
                r.assembly.as_str(),
                csv(&serde_json::to_value(r.integrity)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default()),
                r.sha256.as_deref().unwrap_or(""),
                unreadable,
                r.renamed,
                r.resumed,
                csv(r.error.as_deref().unwrap_or("")),
            )?;
        }
        f.flush()
    }

    pub fn read_json(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }
}

/// Quotes a CSV field when needed and neutralises spreadsheet formula injection.
fn csv(s: &str) -> String {
    let s = if s.starts_with(['=', '+', '-', '@']) {
        format!("'{s}")
    } else {
        s.to_owned()
    };
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::csv;

    #[test]
    fn csv_quoting_and_formula_guard() {
        assert_eq!(csv("plain"), "plain");
        assert_eq!(csv("a,b"), "\"a,b\"");
        assert_eq!(csv("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv("=cmd()"), "'=cmd()");
    }
}
