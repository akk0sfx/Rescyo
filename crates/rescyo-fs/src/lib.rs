//! Shared recovery model for Rescyo's file-system engines.
//!
//! Engines (FAT, NTFS, …) turn a Source into [`VolumeCandidate`]s and then into [`Entry`]s with
//! a [`DataLayout`] that says exactly which bytes make up each file. Recovery never needs an
//! engine again: it copies the extents. Vocabulary follows `CONTEXT.md`.
#![forbid(unsafe_code)]

pub mod codepage;
pub mod content;
mod model;
mod time;

pub use model::*;
pub use time::{TimeZone, Timestamp};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Hard limits that keep hostile structures from exhausting time or memory.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Deepest directory nesting followed.
    pub max_depth: u32,
    /// Most entries accepted from one directory.
    pub max_dir_entries: u32,
    /// Most bytes read for one directory.
    pub max_dir_bytes: u64,
    /// Most entries emitted for one volume.
    pub max_entries: u64,
    /// Most extents in one file's layout before it is cut short.
    pub max_extents: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: 128,
            max_dir_entries: 1 << 20,
            max_dir_bytes: 256 << 20,
            max_entries: 100_000_000,
            max_extents: 1 << 20,
        }
    }
}

/// Cooperative cancellation, checked by engines between units of work.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Receives what an engine finds. Implemented by the Session store and by tests.
pub trait ScanSink {
    /// Stores an entry and returns its id, used as `parent` by its children.
    fn entry(&mut self, entry: Entry) -> Result<EntryId, SinkError>;

    /// Reports progress of the current phase; `total` may be unknown.
    fn progress(&mut self, _phase: Phase, _done: u64, _total: Option<u64>) {}

    /// Reports a noteworthy condition (damage found, limit hit, heuristic used).
    fn note(&mut self, _note: Note) {}
}

/// Why a sink refused an entry.
#[derive(Debug, thiserror::Error)]
#[error("sink error: {0}")]
pub struct SinkError(pub String);

/// An in-memory sink for tests and small volumes.
#[derive(Debug, Default)]
pub struct VecSink {
    pub entries: Vec<Entry>,
    pub notes: Vec<Note>,
}

impl ScanSink for VecSink {
    fn entry(&mut self, entry: Entry) -> Result<EntryId, SinkError> {
        self.entries.push(entry);
        Ok(EntryId(self.entries.len() as u64))
    }

    fn note(&mut self, note: Note) {
        self.notes.push(note);
    }
}

impl VecSink {
    /// The entry with `id` (ids are 1-based positions).
    pub fn get(&self, id: EntryId) -> Option<&Entry> {
        usize::try_from(id.0)
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| self.entries.get(i))
    }

    /// Full path of an entry, joining names up to the root with `/`. Lost entries are
    /// prefixed with `_LOST_FOLDERS/parent_<n>/`.
    pub fn path_of(&self, id: EntryId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(id);
        let mut guard = 0;
        let mut prefix = String::new();
        while let Some(c) = cur {
            guard += 1;
            let Some(e) = self.get(c) else { break };
            if guard > 4096 {
                break;
            }
            parts.push(e.name.clone());
            cur = match e.parent {
                Parent::Entry(p) => Some(p),
                Parent::Root => None,
                Parent::Lost { parent_ref } => {
                    prefix = format!("_LOST_FOLDERS/parent_{parent_ref}/");
                    None
                }
            };
        }
        parts.reverse();
        format!("{prefix}{}", parts.join("/"))
    }
}
