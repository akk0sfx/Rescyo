//! Tree reconstruction from the root directory down.

use std::collections::{HashSet, VecDeque};

use rescyo_fs::{
    AssemblyMethod, CancelToken, DataLayout, Entry, EntryId, EntryKind, EntryState, Limits, Note, Parent,
    Phase, ScanSink, SeedOrigin, SinkError, Times,
};
use rescyo_io::{GuardError, GuardedReader};

use crate::dir::{DirEntry, parse_block};
use crate::inode::{self, Inode, Kind, Source};
use crate::sb::{Ext4Geometry, inode_tables};

const ROOT: u64 = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkStats {
    pub files: u64,
    pub dirs: u64,
    pub deleted: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum WalkError {
    #[error(transparent)]
    Read(#[from] GuardError),
    #[error(transparent)]
    Sink(#[from] SinkError),
    #[error("cancelled")]
    Cancelled,
    #[error("geometry is not usable")]
    InvalidGeometry,
    #[error("the root directory is unreadable")]
    NoRoot,
}

/// Walks one ext2/3/4 volume into `sink`.
pub fn walk(
    r: &GuardedReader,
    g: &Ext4Geometry,
    limits: &Limits,
    sink: &mut dyn ScanSink,
    cancel: &CancelToken,
) -> Result<WalkStats, WalkError> {
    if !g.is_sane() {
        return Err(WalkError::InvalidGeometry);
    }
    if g.needs_recovery {
        sink.note(Note::LimitReached {
            what: "the journal was not replayed: recent changes may be missing".into(),
        });
    }
    if g.encrypted() {
        sink.note(Note::LimitReached {
            what: "encrypted folders have unreadable names and content".into(),
        });
    }
    let tables = inode_tables(r, g)?;
    let root = inode::read(r, g, &tables, ROOT)?
        .filter(|i| i.kind == Kind::Dir)
        .ok_or(WalkError::NoRoot)?;
    let mut stats = WalkStats::default();
    let mut emitted = 0u64;
    let mut seen_dirs: HashSet<u64> = HashSet::from([ROOT]);
    let mut queue: VecDeque<(Inode, Parent, u32, bool)> = VecDeque::from([(root, Parent::Root, 0, false)]);
    while let Some((dir, parent, depth, dir_deleted)) = queue.pop_front() {
        if cancel.is_cancelled() {
            return Err(WalkError::Cancelled);
        }
        sink.progress(Phase::Walk, emitted, None);
        if depth > limits.max_depth {
            sink.note(Note::LimitReached {
                what: "directory depth".into(),
            });
            continue;
        }
        for e in entries(r, g, &dir, limits, sink)? {
            if emitted >= limits.max_entries {
                sink.note(Note::LimitReached {
                    what: "entries".into(),
                });
                return Ok(stats);
            }
            if e.name == "." || e.name == ".." || (e.deleted && e.inode == 0) {
                continue;
            }
            let n = u64::from(e.inode);
            let Some(ino) = inode::read(r, g, &tables, n)? else {
                continue;
            };
            // A deleted name whose inode now belongs to a live file: the name alone is left.
            if e.deleted && !ino.deleted() {
                continue;
            }
            let deleted = e.deleted || dir_deleted || ino.deleted();
            let state = if deleted {
                EntryState::Deleted
            } else {
                EntryState::Live
            };
            match ino.kind {
                Kind::Dir => {
                    if !seen_dirs.insert(n) {
                        continue;
                    }
                    let id = emit(
                        sink,
                        parent,
                        &e,
                        EntryKind::Directory,
                        state,
                        0,
                        ino.times,
                        DataLayout::default(),
                        AssemblyMethod::Ext4Extents,
                        n,
                    )?;
                    emitted += 1;
                    stats.dirs += 1;
                    stats.deleted += u64::from(deleted);
                    queue.push_back((ino, Parent::Entry(id), depth + 1, deleted));
                }
                Kind::File => {
                    let (layout, source) = inode::layout(r, g, &ino, limits.max_extents, deleted)?;
                    let method = match source {
                        Source::BlockMap => AssemblyMethod::BlockMap,
                        Source::Extents | Source::Inline => AssemblyMethod::Ext4Extents,
                    };
                    emit(
                        sink,
                        parent,
                        &e,
                        EntryKind::File,
                        state,
                        ino.size,
                        ino.times,
                        layout,
                        method,
                        n,
                    )?;
                    emitted += 1;
                    stats.files += 1;
                    stats.deleted += u64::from(deleted);
                }
                Kind::Other => {}
            }
        }
    }
    Ok(stats)
}

#[allow(clippy::too_many_arguments, reason = "one argument per entry field")]
fn emit(
    sink: &mut dyn ScanSink,
    parent: Parent,
    e: &DirEntry,
    kind: EntryKind,
    state: EntryState,
    size: u64,
    times: Times,
    layout: DataLayout,
    assembly: AssemblyMethod,
    n: u64,
) -> Result<EntryId, WalkError> {
    Ok(sink.entry(Entry {
        parent,
        name: e.name.clone(),
        name_raw: None,
        kind,
        state,
        size,
        times,
        layout,
        assembly,
        fs_ref: n,
        seed: (kind == EntryKind::Directory).then_some(SeedOrigin::Walk),
    })?)
}

/// Every entry of a directory, live and deleted.
fn entries(
    r: &GuardedReader,
    g: &Ext4Geometry,
    dir: &Inode,
    limits: &Limits,
    sink: &mut dyn ScanSink,
) -> Result<Vec<DirEntry>, WalkError> {
    let (layout, source) = inode::layout(r, g, dir, limits.max_extents, false)?;
    let mut out = Vec::new();
    if source == Source::Inline {
        // Inline directories: a 4-byte parent inode, then entries.
        if let Some(rescyo_fs::Extent::Inline { data }) = layout.extents.first() {
            parse_block(data.get(4..).unwrap_or_default(), true, g.inodes, &mut out);
        }
        return Ok(out);
    }
    let bs = g.block_size as usize;
    let mut read = 0u64;
    let mut index = 0usize;
    for x in &layout.extents {
        let rescyo_fs::Extent::Disk { offset, len } = x else {
            continue;
        };
        let mut at = *offset;
        while at < offset + len {
            if read >= limits.max_dir_bytes {
                sink.note(Note::LimitReached {
                    what: "directory size".into(),
                });
                return Ok(out);
            }
            let mut b = vec![0u8; bs];
            match r.read(at, &mut b) {
                Ok(rep) if rep.is_clean() => {
                    // Block 0 of a hashed directory keeps index data after "..": no names there.
                    let slack = !(dir.hashed_dir() && index == 0);
                    parse_block(&b, slack, g.inodes, &mut out);
                }
                Ok(_) | Err(GuardError::OutOfBounds) => {}
                Err(e) => return Err(e.into()),
            }
            at += bs as u64;
            read += bs as u64;
            index += 1;
            if out.len() as u64 >= u64::from(limits.max_dir_entries) {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

/// Whether the volume `g` describes is really there: its root directory inode reads as a
/// directory where the group descriptors say.
pub fn root_ok(r: &GuardedReader, g: &Ext4Geometry) -> Result<bool, GuardError> {
    if !g.is_sane() {
        return Ok(false);
    }
    let tables = inode_tables(r, g)?;
    Ok(inode::read(r, g, &tables, ROOT)?.is_some_and(|i| i.kind == Kind::Dir && !i.deleted()))
}
