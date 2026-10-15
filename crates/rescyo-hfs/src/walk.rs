//! Catalog walk: folders and files from the leaf chain, deleted file records from the free
//! space of leaf nodes, the tree rebuilt from parent IDs.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use rescyo_fs::{
    AssemblyMethod, CancelToken, DataLayout, Entry, EntryKind, EntryState, Limits, Note, Parent, Phase,
    ScanSink, SeedOrigin, SinkError, Times, Timestamp,
};
use rescyo_io::{GuardError, GuardedReader};
use unicode_normalization::UnicodeNormalization;

use crate::btree::{LEAF, Tree, parse};
use crate::{Fork, HfsGeometry, be16, be32};

/// Data-fork overflow extents by file ID: (first logical block, extents).
type OverflowRun = (u32, Vec<(u32, u32)>);
type Overflow = HashMap<u32, Vec<OverflowRun>>;

const ROOT_FOLDER: u32 = 2;
const CATALOG_FILE: u32 = 4;
const EXTENTS_FILE: u32 = 3;
const FOLDER: u16 = 1;
const FILE: u16 = 2;
/// Seconds from 1904-01-01 to 1970-01-01.
const MAC_EPOCH: i64 = 2_082_844_800;

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
    #[error("the catalog is unreadable")]
    NoCatalog,
}

#[derive(Debug, Clone)]
struct Item {
    parent: u32,
    name: String,
    id: u32,
    folder: bool,
    fork: Fork,
    times: Times,
    deleted: bool,
}

fn date(v: Option<u32>) -> Option<Timestamp> {
    v.filter(|&d| d != 0)
        .and_then(|d| Timestamp::from_unix(i64::from(d) - MAC_EPOCH, 0))
}

/// Parses one catalog record (key and data). `None` for thread records and garbage.
fn record(r: &[u8]) -> Option<Item> {
    let key_len = usize::from(be16(r, 0)?);
    if !(6..=516).contains(&key_len) {
        return None;
    }
    let parent = be32(r, 2)?;
    let name_len = usize::from(be16(r, 6)?);
    if 6 + name_len * 2 > key_len || name_len > 255 {
        return None;
    }
    let units: Vec<u16> = (0..name_len).filter_map(|i| be16(r, 8 + i * 2)).collect();
    let name: String = String::from_utf16(&units).ok()?.nfc().collect();
    let mut d = 2 + key_len;
    d += d & 1;
    let data = r.get(d..)?;
    let kind = be16(data, 0)?;
    let times = |c: usize, m: usize, a: usize| Times {
        created: date(be32(data, c)),
        modified: date(be32(data, m)),
        accessed: date(be32(data, a)),
    };
    match kind {
        FOLDER => Some(Item {
            parent,
            name,
            id: be32(data, 8)?,
            folder: true,
            fork: Fork::default(),
            times: times(12, 16, 24),
            deleted: false,
        }),
        FILE => Some(Item {
            parent,
            name,
            id: be32(data, 8)?,
            folder: false,
            fork: Fork::parse(data, 88)?,
            times: times(12, 16, 24),
            deleted: false,
        }),
        _ => None,
    }
}

/// Every data-fork overflow extent, by file ID: (first logical block, extents).
fn overflow(r: &GuardedReader, g: &HfsGeometry) -> Result<Overflow, GuardError> {
    let mut out: Overflow = HashMap::new();
    let Some(t) = Tree::open(r, g, &g.extents_file, &[])? else {
        return Ok(out);
    };
    let mut n = t.first_leaf;
    let mut seen = HashSet::new();
    while n != 0 && seen.insert(n) && seen.len() <= t.total_nodes as usize {
        let Some(b) = t.node(n)? else { break };
        let Some(node) = parse(&b) else { break };
        for rec in &node.records {
            // Key: length 10, fork type, pad, file ID, start block; then 8 extents.
            let (Some(10), Some(&0)) = (be16(rec, 0), rec.get(2)) else {
                continue;
            };
            let (Some(id), Some(start)) = (be32(rec, 4), be32(rec, 8)) else {
                continue;
            };
            let ext: Vec<(u32, u32)> = (0..8)
                .filter_map(|i| Some((be32(rec, 12 + i * 8)?, be32(rec, 16 + i * 8)?)))
                .filter(|&(_, c)| c > 0)
                .collect();
            out.entry(id).or_default().push((start, ext));
        }
        n = node.next;
    }
    for v in out.values_mut() {
        v.sort_by_key(|x| x.0);
    }
    Ok(out)
}

/// Deleted file records in the free space of a leaf node: a deletion shifts the following
/// records down and leaves the old bytes behind.
fn slack_records(free: &[u8], out: &mut Vec<Item>) {
    let mut p = 0usize;
    while p + 8 < free.len() {
        if let Some(mut it) = free.get(p..).and_then(record)
            && !it.folder
            && it.parent >= 1
            && it.id > 15
            && !it.name.is_empty()
            && !it.name.contains(['\0', '/'])
        {
            it.deleted = true;
            let key_len = usize::from(be16(free, p).unwrap_or(0));
            out.push(it);
            p += 2 + key_len;
            continue;
        }
        p += 2;
    }
}

/// Walks one HFS+ volume into `sink`.
pub fn walk(
    r: &GuardedReader,
    g: &HfsGeometry,
    limits: &Limits,
    sink: &mut dyn ScanSink,
    cancel: &CancelToken,
) -> Result<WalkStats, WalkError> {
    let extra = overflow(r, g)?;
    let cat_extra: Vec<(u32, u32)> = extra
        .get(&CATALOG_FILE)
        .map(|v| v.iter().flat_map(|x| x.1.clone()).collect())
        .unwrap_or_default();
    let tree = Tree::open(r, g, &g.catalog, &cat_extra)?.ok_or(WalkError::NoCatalog)?;
    let mut items: Vec<Item> = Vec::new();
    let mut live_ids: HashSet<u32> = HashSet::new();
    // Live records: the leaf chain.
    let mut chain = HashSet::new();
    let mut n = tree.first_leaf;
    while n != 0 && chain.insert(n) && chain.len() <= tree.total_nodes as usize {
        if cancel.is_cancelled() {
            return Err(WalkError::Cancelled);
        }
        let Some(b) = tree.node(n)? else { break };
        let Some(node) = parse(&b) else { break };
        if node.kind != LEAF {
            break;
        }
        for rec in &node.records {
            if let Some(it) = record(rec) {
                live_ids.insert(it.id);
                items.push(it);
            }
        }
        n = node.next;
    }
    if items.is_empty() {
        return Err(WalkError::NoCatalog);
    }
    // Deleted records: free space of every leaf node, and stale leaf nodes off the chain.
    let mut deleted = Vec::new();
    for i in 0..tree.total_nodes {
        if i % 4096 == 0 {
            if cancel.is_cancelled() {
                return Err(WalkError::Cancelled);
            }
            sink.progress(Phase::Walk, u64::from(i), Some(u64::from(tree.total_nodes)));
        }
        let Some(b) = tree.node(i)? else { continue };
        let Some(node) = parse(&b) else { continue };
        if node.kind != LEAF {
            continue;
        }
        slack_records(node.free, &mut deleted);
        if !chain.contains(&i) {
            for rec in &node.records {
                if let Some(mut it) = record(rec).filter(|it| !it.folder) {
                    it.deleted = true;
                    deleted.push(it);
                }
            }
        }
    }
    let mut seen_deleted = HashSet::new();
    for it in deleted {
        if !live_ids.contains(&it.id) && seen_deleted.insert(it.id) {
            items.push(it);
        }
    }

    let folders: HashMap<u32, usize> = items
        .iter()
        .enumerate()
        .filter(|(_, i)| i.folder)
        .map(|(k, i)| (i.id, k))
        .collect();
    let mut children: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (k, it) in items.iter().enumerate() {
        if it.id != ROOT_FOLDER && it.id != CATALOG_FILE && it.id != EXTENTS_FILE {
            children.entry(it.parent).or_default().push(k);
        }
    }
    let mut stats = WalkStats::default();
    let mut emitted = 0u64;
    let mut done: HashSet<u32> = HashSet::from([ROOT_FOLDER]);
    let mut queue: VecDeque<(u32, Parent, u32)> = VecDeque::from([(ROOT_FOLDER, Parent::Root, 0)]);
    let emit_folder_tree = |queue: &mut VecDeque<(u32, Parent, u32)>,
                            stats: &mut WalkStats,
                            emitted: &mut u64,
                            done: &mut HashSet<u32>,
                            sink: &mut dyn ScanSink|
     -> Result<(), WalkError> {
        while let Some((dir, parent, depth)) = queue.pop_front() {
            if depth > limits.max_depth {
                sink.note(Note::LimitReached {
                    what: "directory depth".into(),
                });
                continue;
            }
            for &k in children.get(&dir).map(Vec::as_slice).unwrap_or_default() {
                let Some(it) = items.get(k) else { continue };
                if *emitted >= limits.max_entries {
                    return Ok(());
                }
                // The private folders of hard links and the HFS+ metadata are not user files.
                if it.name.starts_with('\0') || it.name.starts_with(".HFS+ Private Directory Data") {
                    continue;
                }
                let state = if it.deleted {
                    EntryState::Deleted
                } else {
                    EntryState::Live
                };
                if it.folder {
                    if !done.insert(it.id) {
                        continue;
                    }
                    let id = sink.entry(entry(
                        it,
                        parent,
                        EntryKind::Directory,
                        DataLayout::default(),
                        state,
                    ))?;
                    *emitted += 1;
                    stats.dirs += 1;
                    queue.push_back((it.id, Parent::Entry(id), depth + 1));
                } else {
                    let layout = data_layout(g, it, extra.get(&it.id), limits.max_extents);
                    sink.entry(entry(it, parent, EntryKind::File, layout, state))?;
                    *emitted += 1;
                    stats.files += 1;
                    stats.deleted += u64::from(it.deleted);
                }
            }
        }
        Ok(())
    };
    emit_folder_tree(&mut queue, &mut stats, &mut emitted, &mut done, sink)?;
    // Records whose parent folder is gone: under _LOST_FOLDERS by the parent ID.
    let orphans: Vec<u32> = children
        .keys()
        .copied()
        .filter(|p| !done.contains(p) && !folders.contains_key(p) && *p > 1)
        .collect();
    for p in orphans {
        queue.push_back((
            p,
            Parent::Lost {
                parent_ref: u64::from(p),
            },
            1,
        ));
        done.insert(p);
        emit_folder_tree(&mut queue, &mut stats, &mut emitted, &mut done, sink)?;
    }
    Ok(stats)
}

fn entry(it: &Item, parent: Parent, kind: EntryKind, layout: DataLayout, state: EntryState) -> Entry {
    Entry {
        parent,
        name: it.name.clone(),
        name_raw: None,
        kind,
        state,
        size: if kind == EntryKind::File { it.fork.size } else { 0 },
        times: it.times,
        layout,
        assembly: AssemblyMethod::HfsExtents,
        fs_ref: u64::from(it.id),
        seed: (kind == EntryKind::Directory).then_some(SeedOrigin::Walk),
    }
}

fn data_layout(g: &HfsGeometry, it: &Item, overflow: Option<&Vec<OverflowRun>>, max: u32) -> DataLayout {
    let bs = u64::from(g.block_size);
    let mut out = DataLayout::default();
    let mut left = it.fork.size;
    let more = overflow
        .map(|v| v.iter().flat_map(|x| x.1.iter().copied()).collect::<Vec<_>>())
        .unwrap_or_default();
    for (i, (start, count)) in it.fork.extents.iter().copied().chain(more).enumerate() {
        if left == 0 || i as u64 >= u64::from(max) {
            break;
        }
        let Some(off) = g.block_offset(start) else { break };
        if u64::from(start) + u64::from(count) > u64::from(g.total_blocks) {
            break;
        }
        let len = (u64::from(count) * bs).min(left);
        out.push_disk(off, len);
        left -= len;
    }
    out
}

#[cfg(test)]
type Parsed = (String, u32, u32, u64, Vec<(u32, u32)>);

#[cfg(test)]
pub(crate) fn tests_record(r: &[u8]) -> Option<Parsed> {
    record(r).map(|i| (i.name, i.parent, i.id, i.fork.size, i.fork.extents))
}

#[cfg(test)]
pub(crate) fn tests_slack(free: &[u8]) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    slack_records(free, &mut out);
    out.into_iter().map(|i| (i.name, i.id)).collect()
}
