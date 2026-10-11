//! The machine-readable output contract (`--json`), schema version 1.
//!
//! Every command prints one envelope: `{"schema_version":1,"command":…,"ok":true,"data":…}` or
//! `{"schema_version":1,"command":…,"ok":false,"error":{"code":…,"message":…,"details":…}}`.
//! Adding fields is compatible; removing or renaming one requires schema version 2. The JSON
//! Schemas in `schemas/v1/` are generated from these types and checked by a test.

use schemars::JsonSchema;
use serde::Serialize;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, JsonSchema)]
pub struct Envelope<T: Serialize> {
    pub schema_version: u32,
    pub command: &'static str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ErrorBody {
    /// Stable snake_case identifier, e.g. `target_on_source_device`.
    pub code: String,
    /// English description; never localized.
    pub message: String,
    pub details: serde_json::Value,
}

/// One line of `--json-stream` output before the final envelope.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    Progress {
        phase: String,
        done: u64,
        total: Option<u64>,
    },
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Disk {
    pub path: String,
    pub size: Option<u64>,
    pub logical_sector: Option<u32>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub removable: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DisksData {
    pub disks: Vec<Disk>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Volume {
    pub id: i64,
    pub fs: String,
    pub offset: Option<u64>,
    pub cluster_size: u32,
    pub data_offset: u64,
    pub score: u64,
    pub walked: bool,
    pub files: u64,
    pub dirs: u64,
    pub deleted: u64,
    pub lost_folders: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ScanData {
    pub session: String,
    pub partition_table: String,
    pub mode: String,
    pub volumes: Vec<Volume>,
    pub unreadable_sectors: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ImportData {
    pub dmde_volume: u32,
    pub cluster_size: u32,
    pub data_offset: u64,
    pub dmde_folders: u64,
    pub seeds_used: u64,
    pub seeds_off_grid: u64,
    pub warnings: Vec<String>,
    pub scan: ScanData,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct VolumeInfo {
    pub id: i64,
    pub fs: String,
    pub offset: Option<u64>,
    pub cluster_size: u32,
    pub data_offset: u64,
    pub size: Option<u64>,
    pub label: Option<String>,
    pub score: u64,
    pub walked: bool,
    /// Evidence for the geometry, as tagged objects (`boot_sector`, `fat_signature`, …).
    pub evidence: serde_json::Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct VolumesData {
    pub volumes: Vec<VolumeInfo>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EntryRow {
    pub id: u64,
    pub volume: i64,
    pub path: String,
    /// `file` or `directory`.
    /// `file`, `directory` or `stream` (a named stream, shown as `file:stream`).
    pub kind: String,
    /// `live` or `deleted`.
    pub state: String,
    pub size: u64,
    /// RFC 3339; local-zone stamps (FAT) carry no offset.
    pub modified: Option<String>,
    /// How the bytes were found: `fat_chain`, `fat_chain_mirror`, `mft_runlist`, …
    pub assembly: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LsData {
    pub entries: Vec<EntryRow>,
    pub total: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
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

#[derive(Debug, Serialize, JsonSchema)]
pub struct RecoverData {
    pub target: String,
    pub report_json: String,
    pub report_csv: String,
    pub target_unverified: bool,
    pub source_unverified: bool,
    pub totals: Totals,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct JournalRow {
    pub time: i64,
    pub action: String,
    pub detail: serde_json::Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InfoData {
    pub session: String,
    pub source_path: String,
    pub source_kind: String,
    pub source_size: u64,
    pub source_unverified: bool,
    pub volumes: u64,
    pub entries: u64,
    pub unreadable_sectors: u64,
    pub journal: Vec<JournalRow>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ImageData {
    pub source: String,
    pub image: String,
    /// The ddrescue mapfile describing which parts of the image hold real data.
    pub map: String,
    pub size: u64,
    pub rescued: u64,
    /// Bytes that could not be read; zeros in the image.
    pub bad_bytes: u64,
    pub bad_areas: u64,
    /// The run continued an earlier, interrupted copy.
    pub resumed: bool,
}

/// Every schema, by command name, for the schema test (which can also rewrite them).
#[cfg_attr(not(test), allow(dead_code))]
pub fn schemas() -> Vec<(&'static str, serde_json::Value)> {
    fn s<T: JsonSchema>() -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(T)).unwrap_or_default()
    }
    vec![
        ("envelope-error", s::<Envelope<()>>()),
        ("stream-event", s::<StreamEvent>()),
        ("disks", s::<DisksData>()),
        ("scan", s::<ScanData>()),
        ("import-dmde", s::<ImportData>()),
        ("volumes", s::<VolumesData>()),
        ("ls", s::<LsData>()),
        ("recover", s::<RecoverData>()),
        ("info", s::<InfoData>()),
        ("image", s::<ImageData>()),
    ]
}
