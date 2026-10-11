//! The Session: one SQLite file holding everything found on one Source.

use std::path::{Path, PathBuf};

use rescyo_fs::{
    AssemblyMethod, DataLayout, Entry, EntryId, EntryKind, EntryState, Extent, Note, Parent, Phase, ScanSink,
    SeedOrigin, SinkError, TimeZone, Times, Timestamp, VolumeCandidate,
};
use rescyo_io::{BadSectorMap, FingerprintMatch, Sample, SourceFingerprint, SourceIdentity, SourceKind};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

/// `RESC` in the SQLite header, so `file` and Rescyo itself recognise a Session.
const APPLICATION_ID: i32 = 0x5245_5343;
const SCHEMA_VERSION: i32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("{0} already exists; refusing to overwrite a session")]
    Exists(PathBuf),
    #[error("{0} is not a Rescyo session")]
    NotASession(PathBuf),
    #[error("session schema {found} is newer than this version of Rescyo understands ({supported})")]
    TooNew { found: i32, supported: i32 },
    #[error("corrupt session data: {0}")]
    Corrupt(String),
}

pub type Result<T> = std::result::Result<T, SessionError>;

/// The Source a Session belongs to, as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub identity: SourceIdentity,
    pub fingerprint: SourceFingerprint,
    /// The Session was rebound to a different Source whose fingerprint did not match.
    pub forced: bool,
}

/// A volume row: the candidate plus whether it was walked.
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeRow {
    pub id: i64,
    pub candidate: VolumeCandidate,
    pub walked: bool,
}

/// An entry as stored, with its id and volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEntry {
    pub id: EntryId,
    pub volume: i64,
    pub entry: Entry,
}

#[derive(Debug)]
pub struct Session {
    conn: Connection,
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct FingerprintDto {
    size: u64,
    logical_sector: u32,
    model: Option<String>,
    serial: Option<String>,
    samples: Vec<(u64, u64, Option<String>)>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    if s.len() != 64 {
        return None;
    }
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn fp_to_json(fp: &SourceFingerprint) -> String {
    let dto = FingerprintDto {
        size: fp.size,
        logical_sector: fp.logical_sector,
        model: fp.model.clone(),
        serial: fp.serial.clone(),
        samples: fp
            .samples
            .iter()
            .map(|s| (s.offset, s.len, s.sha256.map(|h| hex(&h))))
            .collect(),
    };
    serde_json::to_string(&dto).unwrap_or_default()
}

fn fp_from_json(s: &str) -> Result<SourceFingerprint> {
    let dto: FingerprintDto = serde_json::from_str(s).map_err(|e| SessionError::Corrupt(e.to_string()))?;
    Ok(SourceFingerprint {
        size: dto.size,
        logical_sector: dto.logical_sector,
        model: dto.model,
        serial: dto.serial,
        samples: dto
            .samples
            .into_iter()
            .map(|(offset, len, h)| Sample {
                offset,
                len,
                sha256: h.as_deref().and_then(unhex),
            })
            .collect(),
    })
}

fn kind_str(k: SourceKind) -> &'static str {
    match k {
        SourceKind::Image => "image",
        SourceKind::PhysicalDisk => "physical_disk",
        SourceKind::Test => "test",
    }
}

fn kind_from(s: &str) -> SourceKind {
    match s {
        "image" => SourceKind::Image,
        "physical_disk" => SourceKind::PhysicalDisk,
        _ => SourceKind::Test,
    }
}

fn i(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn u(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

const SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE source (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  kind TEXT NOT NULL, path TEXT NOT NULL, model TEXT, serial TEXT,
  fingerprint TEXT NOT NULL, forced INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE volumes (
  id INTEGER PRIMARY KEY, candidate TEXT NOT NULL, walked INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE entries (
  id INTEGER PRIMARY KEY,
  volume_id INTEGER NOT NULL REFERENCES volumes(id),
  parent_kind TEXT NOT NULL, parent_id INTEGER, lost_ref INTEGER,
  name TEXT NOT NULL, name_raw BLOB,
  kind TEXT NOT NULL, state TEXT NOT NULL, size INTEGER NOT NULL,
  c_unix INTEGER, c_ns INTEGER, c_zone TEXT,
  m_unix INTEGER, m_ns INTEGER, m_zone TEXT,
  a_unix INTEGER, a_ns INTEGER, a_zone TEXT,
  assembly TEXT NOT NULL, fs_ref INTEGER NOT NULL, seed TEXT
);
CREATE INDEX entries_parent ON entries(volume_id, parent_id);
CREATE INDEX entries_name ON entries(name);
CREATE TABLE extents (
  entry_id INTEGER NOT NULL, seq INTEGER NOT NULL,
  kind TEXT NOT NULL, offset INTEGER, len INTEGER NOT NULL, data BLOB,
  PRIMARY KEY (entry_id, seq)
) WITHOUT ROWID;
CREATE TABLE bad_sectors (start INTEGER PRIMARY KEY, end INTEGER NOT NULL) WITHOUT ROWID;
CREATE TABLE journal (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, action TEXT NOT NULL, detail TEXT NOT NULL);
";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

impl Session {
    /// Creates a new Session for a Source. Never overwrites an existing file.
    pub fn create(path: &Path, identity: &SourceIdentity, fingerprint: &SourceFingerprint) -> Result<Self> {
        if path.exists() {
            return Err(SessionError::Exists(path.to_path_buf()));
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "DELETE")?;
        conn.pragma_update(None, "application_id", APPLICATION_ID)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('tool_version', ?1), ('created', ?2)",
            params![env!("CARGO_PKG_VERSION"), now().to_string()],
        )?;
        conn.execute(
            "INSERT INTO source (id, kind, path, model, serial, fingerprint) VALUES (1, ?1, ?2, ?3, ?4, ?5)",
            params![
                kind_str(identity.kind),
                identity.path,
                identity.model,
                identity.serial,
                fp_to_json(fingerprint)
            ],
        )?;
        let s = Self {
            conn,
            path: path.to_path_buf(),
        };
        s.journal("session_created", &serde_json::json!({ "source": identity.path }))?;
        Ok(s)
    }

    /// Opens an existing Session, checking that it is one and that its schema is understood.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Err(SessionError::NotASession(path.to_path_buf()));
        }
        let conn = Connection::open(path)?;
        let app: i32 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if app != APPLICATION_ID {
            return Err(SessionError::NotASession(path.to_path_buf()));
        }
        let ver: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if ver > SCHEMA_VERSION {
            return Err(SessionError::TooNew {
                found: ver,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn source(&self) -> Result<SourceRecord> {
        let (kind, path, model, serial, fp, forced): (
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            bool,
        ) = self.conn.query_row(
            "SELECT kind, path, model, serial, fingerprint, forced FROM source WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )?;
        Ok(SourceRecord {
            identity: SourceIdentity {
                kind: kind_from(&kind),
                path,
                model,
                serial,
            },
            fingerprint: fp_from_json(&fp)?,
            forced,
        })
    }

    /// Compares the recorded fingerprint with one computed from the Source in front of us.
    pub fn check_source(&self, fresh: &SourceFingerprint) -> Result<FingerprintMatch> {
        Ok(self.source()?.fingerprint.compare(fresh))
    }

    /// Attaches the Session to another Source (usually an image of the original disk).
    pub fn rebind(&self, identity: &SourceIdentity, forced: bool) -> Result<()> {
        let old = self.source()?;
        self.conn.execute(
            "UPDATE source SET kind = ?1, path = ?2, model = ?3, serial = ?4, forced = forced OR ?5 WHERE id = 1",
            params![kind_str(identity.kind), identity.path, identity.model, identity.serial, forced],
        )?;
        self.journal(
            if forced {
                "source_rebound_forced"
            } else {
                "source_rebound"
            },
            &serde_json::json!({ "from": old.identity.path, "to": identity.path }),
        )
    }

    pub fn journal(&self, action: &str, detail: &serde_json::Value) -> Result<()> {
        self.conn.execute(
            "INSERT INTO journal (ts, action, detail) VALUES (?1, ?2, ?3)",
            params![now(), action, detail.to_string()],
        )?;
        Ok(())
    }

    /// Journal rows, oldest first: (unix time, action, detail).
    pub fn journal_entries(&self) -> Result<Vec<(i64, String, serde_json::Value)>> {
        let mut st = self
            .conn
            .prepare("SELECT ts, action, detail FROM journal ORDER BY id")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|r| {
            let (ts, a, d) = r?;
            Ok((ts, a, serde_json::from_str(&d).unwrap_or(serde_json::Value::Null)))
        })
        .collect()
    }

    pub fn add_volume(&self, c: &VolumeCandidate) -> Result<i64> {
        let json = serde_json::to_string(c).map_err(|e| SessionError::Corrupt(e.to_string()))?;
        self.conn
            .execute("INSERT INTO volumes (candidate) VALUES (?1)", params![json])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn mark_walked(&self, volume: i64) -> Result<()> {
        self.conn
            .execute("UPDATE volumes SET walked = 1 WHERE id = ?1", params![volume])?;
        Ok(())
    }

    pub fn volumes(&self) -> Result<Vec<VolumeRow>> {
        let mut st = self
            .conn
            .prepare("SELECT id, candidate, walked FROM volumes ORDER BY id")?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?))
        })?;
        rows.map(|r| {
            let (id, c, walked) = r?;
            let candidate = serde_json::from_str(&c).map_err(|e| SessionError::Corrupt(e.to_string()))?;
            Ok(VolumeRow {
                id,
                candidate,
                walked,
            })
        })
        .collect()
    }

    pub fn save_bad_map(&self, map: &BadSectorMap) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM bad_sectors", [])?;
        {
            let mut st = tx.prepare("INSERT INTO bad_sectors (start, end) VALUES (?1, ?2)")?;
            for r in map.iter() {
                st.execute(params![i(r.start), i(r.end)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn bad_map(&self) -> Result<BadSectorMap> {
        let mut st = self
            .conn
            .prepare("SELECT start, end FROM bad_sectors ORDER BY start")?;
        let rows = st.query_map([], |r| Ok(u(r.get(0)?)..u(r.get(1)?)))?;
        rows.collect::<std::result::Result<BadSectorMap, _>>()
            .map_err(Into::into)
    }

    /// A sink that stores a volume's entries in batches inside one transaction.
    pub fn sink(&mut self, volume: i64) -> Result<SessionSink<'_>> {
        let tx = self.conn.transaction()?;
        Ok(SessionSink {
            tx: Some(tx),
            volume,
            notes: Vec::new(),
            progress: None,
        })
    }

    pub fn entry_count(&self) -> Result<u64> {
        Ok(u(self.conn.query_row(
            "SELECT COUNT(*) FROM entries",
            [],
            |r| r.get(0),
        )?))
    }

    /// Loads entries matching an SQL-free filter (applied in Rust over a streamed query).
    pub fn entries(&self, mut keep: impl FnMut(&StoredEntry) -> bool) -> Result<Vec<StoredEntry>> {
        let mut st = self.conn.prepare(
            "SELECT id, volume_id, parent_kind, parent_id, lost_ref, name, name_raw, kind, state, size,
                    c_unix, c_ns, c_zone, m_unix, m_ns, m_zone, a_unix, a_ns, a_zone, assembly, fs_ref, seed
             FROM entries ORDER BY id",
        )?;
        let mut ext = self
            .conn
            .prepare("SELECT kind, offset, len, data FROM extents WHERE entry_id = ?1 ORDER BY seq")?;
        let mut rows = st.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let mut e = row_to_entry(r)?;
            if !keep(&e) {
                continue;
            }
            let id = i(e.id.0);
            let mut er = ext.query(params![id])?;
            while let Some(x) = er.next()? {
                let kind: String = x.get(0)?;
                let len = u(x.get(2)?);
                e.entry.layout.extents.push(match kind.as_str() {
                    "disk" => Extent::Disk {
                        offset: u(x.get::<_, Option<i64>>(1)?.unwrap_or(0)),
                        len,
                    },
                    "inline" => Extent::Inline {
                        data: x.get::<_, Option<Vec<u8>>>(3)?.unwrap_or_default(),
                    },
                    _ => Extent::Zero { len },
                });
            }
            out.push(e);
        }
        Ok(out)
    }

    /// Full path of an entry from its volume root; lost entries start with
    /// `_LOST_FOLDERS/parent_<n>/`.
    pub fn path_of(&self, id: EntryId) -> Result<String> {
        let mut parts = Vec::new();
        let mut cur = Some(i(id.0));
        let mut prefix = String::new();
        let mut guard = 0;
        while let Some(c) = cur {
            guard += 1;
            if guard > 4096 {
                return Err(SessionError::Corrupt("parent cycle".into()));
            }
            let row: Option<(String, String, Option<i64>, Option<i64>)> = self
                .conn
                .query_row(
                    "SELECT name, parent_kind, parent_id, lost_ref FROM entries WHERE id = ?1",
                    params![c],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let Some((name, pk, pid, lost)) = row else { break };
            parts.push(name);
            cur = match pk.as_str() {
                "entry" => pid,
                "lost" => {
                    prefix = format!("_LOST_FOLDERS/parent_{}/", lost.unwrap_or(0));
                    None
                }
                _ => None,
            };
        }
        parts.reverse();
        Ok(format!("{prefix}{}", parts.join("/")))
    }
}

fn ts_parts(t: Option<Timestamp>) -> (Option<i64>, Option<i64>, Option<&'static str>) {
    match t {
        Some(t) => (
            Some(t.unix),
            Some(i64::from(t.nanos)),
            Some(match t.zone {
                TimeZone::Utc => "utc",
                TimeZone::Local => "local",
            }),
        ),
        None => (None, None, None),
    }
}

fn ts_from(unix: Option<i64>, ns: Option<i64>, zone: Option<String>) -> Option<Timestamp> {
    Some(Timestamp {
        unix: unix?,
        nanos: u32::try_from(ns.unwrap_or(0)).unwrap_or(0),
        zone: if zone.as_deref() == Some("utc") {
            TimeZone::Utc
        } else {
            TimeZone::Local
        },
    })
}

fn enum_str<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn enum_from<T: for<'de> Deserialize<'de>>(s: &str) -> rusqlite::Result<T> {
    serde_json::from_value(serde_json::Value::String(s.to_owned()))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

fn row_to_entry(r: &rusqlite::Row<'_>) -> Result<StoredEntry> {
    let id: i64 = r.get(0)?;
    let pk: String = r.get(2)?;
    let parent = match pk.as_str() {
        "entry" => Parent::Entry(EntryId(u(r.get::<_, Option<i64>>(3)?.unwrap_or(0)))),
        "lost" => Parent::Lost {
            parent_ref: u(r.get::<_, Option<i64>>(4)?.unwrap_or(0)),
        },
        _ => Parent::Root,
    };
    let kind: EntryKind = enum_from(&r.get::<_, String>(7)?)?;
    let state: EntryState = enum_from(&r.get::<_, String>(8)?)?;
    let assembly: AssemblyMethod = enum_from(&r.get::<_, String>(19)?)?;
    let seed: Option<SeedOrigin> = match r.get::<_, Option<String>>(21)? {
        Some(s) => Some(enum_from(&s)?),
        None => None,
    };
    Ok(StoredEntry {
        id: EntryId(u(id)),
        volume: r.get(1)?,
        entry: Entry {
            parent,
            name: r.get(5)?,
            name_raw: r.get(6)?,
            kind,
            state,
            size: u(r.get(9)?),
            times: Times {
                created: ts_from(r.get(10)?, r.get(11)?, r.get(12)?),
                modified: ts_from(r.get(13)?, r.get(14)?, r.get(15)?),
                accessed: ts_from(r.get(16)?, r.get(17)?, r.get(18)?),
            },
            layout: DataLayout::default(),
            assembly,
            fs_ref: u(r.get(20)?),
            seed,
        },
    })
}

/// Stores entries for one volume. Commits on [`SessionSink::finish`]; dropping it without
/// finishing rolls the volume's entries back.
pub struct SessionSink<'a> {
    tx: Option<Transaction<'a>>,
    volume: i64,
    notes: Vec<Note>,
    progress: Option<ProgressFn<'a>>,
}

type ProgressFn<'a> = Box<dyn FnMut(Phase, u64, Option<u64>) + 'a>;

impl std::fmt::Debug for SessionSink<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionSink")
            .field("volume", &self.volume)
            .finish_non_exhaustive()
    }
}

impl<'a> SessionSink<'a> {
    /// Forwards progress reports to `f` (for the CLI or GUI).
    pub fn on_progress(&mut self, f: impl FnMut(Phase, u64, Option<u64>) + 'a) {
        self.progress = Some(Box::new(f));
    }

    /// Commits the entries and records the engine's notes in the journal.
    pub fn finish(mut self) -> Result<Vec<Note>> {
        let notes = std::mem::take(&mut self.notes);
        if let Some(tx) = self.tx.take() {
            for n in &notes {
                tx.execute(
                    "INSERT INTO journal (ts, action, detail) VALUES (?1, 'engine_note', ?2)",
                    params![now(), serde_json::to_string(n).unwrap_or_default()],
                )?;
            }
            tx.commit()?;
        }
        Ok(notes)
    }

    fn insert(&mut self, e: &Entry) -> std::result::Result<i64, rusqlite::Error> {
        let Some(tx) = self.tx.as_ref() else {
            return Err(rusqlite::Error::InvalidQuery);
        };
        let (pk, pid, lost) = match e.parent {
            Parent::Root => ("root", None, None),
            Parent::Entry(id) => ("entry", Some(i(id.0)), None),
            Parent::Lost { parent_ref } => ("lost", None, Some(i(parent_ref))),
        };
        let (cu, cn, cz) = ts_parts(e.times.created);
        let (mu, mn, mz) = ts_parts(e.times.modified);
        let (au, an, az) = ts_parts(e.times.accessed);
        tx.prepare_cached(
            "INSERT INTO entries (volume_id, parent_kind, parent_id, lost_ref, name, name_raw, kind, state, size,
               c_unix, c_ns, c_zone, m_unix, m_ns, m_zone, a_unix, a_ns, a_zone, assembly, fs_ref, seed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
        )?
        .execute(params![
            self.volume,
            pk,
            pid,
            lost,
            e.name,
            e.name_raw,
            enum_str(&e.kind),
            enum_str(&e.state),
            i(e.size),
            cu,
            cn,
            cz,
            mu,
            mn,
            mz,
            au,
            an,
            az,
            enum_str(&e.assembly),
            i(e.fs_ref),
            e.seed.as_ref().map(enum_str),
        ])?;
        let id = tx.last_insert_rowid();
        let mut st = tx.prepare_cached(
            "INSERT INTO extents (entry_id, seq, kind, offset, len, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (seq, x) in e.layout.extents.iter().enumerate() {
            let seq = i64::try_from(seq).unwrap_or(i64::MAX);
            match x {
                Extent::Disk { offset, len } => {
                    st.execute(params![id, seq, "disk", i(*offset), i(*len), None::<Vec<u8>>])?
                }
                Extent::Zero { len } => {
                    st.execute(params![id, seq, "zero", None::<i64>, i(*len), None::<Vec<u8>>])?
                }
                Extent::Inline { data } => st.execute(params![
                    id,
                    seq,
                    "inline",
                    None::<i64>,
                    i(data.len() as u64),
                    data
                ])?,
            };
        }
        Ok(id)
    }
}

impl ScanSink for SessionSink<'_> {
    fn entry(&mut self, entry: Entry) -> std::result::Result<EntryId, SinkError> {
        self.insert(&entry)
            .map(|id| EntryId(u(id)))
            .map_err(|e| SinkError(e.to_string()))
    }

    fn progress(&mut self, phase: Phase, done: u64, total: Option<u64>) {
        if let Some(f) = self.progress.as_mut() {
            f(phase, done, total);
        }
    }

    fn note(&mut self, note: Note) {
        if self.notes.len() < 10_000 {
            self.notes.push(note);
        }
    }
}
