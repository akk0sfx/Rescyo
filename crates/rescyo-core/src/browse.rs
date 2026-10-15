//! What an interface shows, built once from a Session (shared by the TUI and the GUI): the folder tree of every volume (lost
//! folders gathered under `_LOST_FOLDERS`), file listings, search and the marked files.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::session::{Session, SessionError, StoredEntry, VolumeRow};
use rescyo_fs::{EntryKind, EntryState, Parent};

pub const LOST: &str = "_LOST_FOLDERS";

/// A place in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Node {
    Volume(i64),
    Lost(i64),
    Dir(u64),
}

/// One visible row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub node: Node,
    pub depth: u16,
    pub label: String,
    pub expandable: bool,
    pub expanded: bool,
    pub lost: bool,
    pub deleted: bool,
}

#[derive(Debug)]
pub struct Model {
    pub entries: Vec<StoredEntry>,
    pub volumes: Vec<VolumeRow>,
    by_id: HashMap<u64, usize>,
    children: HashMap<Node, Vec<usize>>,
    pub expanded: HashSet<Node>,
    pub marked: BTreeSet<u64>,
    pub show_deleted: bool,
    pub show_streams: bool,
}

impl Model {
    pub fn load(s: &Session) -> Result<Self, SessionError> {
        let volumes = s.volumes()?;
        let entries = s.entries(|_| true)?;
        Ok(Self::from_parts(volumes, entries))
    }

    pub fn from_parts(volumes: Vec<VolumeRow>, entries: Vec<StoredEntry>) -> Self {
        let by_id: HashMap<u64, usize> = entries.iter().enumerate().map(|(i, e)| (e.id.0, i)).collect();
        let mut children: HashMap<Node, Vec<usize>> = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            let node = match e.entry.parent {
                Parent::Root => Node::Volume(e.volume),
                Parent::Entry(p) if by_id.contains_key(&p.0) => Node::Dir(p.0),
                Parent::Entry(_) | Parent::Lost { .. } => Node::Lost(e.volume),
            };
            children.entry(node).or_default().push(i);
        }
        for list in children.values_mut() {
            // Folders first, then by name.
            list.sort_by(|&a, &b| {
                let key = |i: usize| {
                    entries
                        .get(i)
                        .map(|e| (e.entry.kind != EntryKind::Directory, e.entry.name.to_lowercase()))
                };
                key(a).cmp(&key(b))
            });
        }
        let mut expanded = HashSet::new();
        if let Some(v) = volumes.iter().find(|v| v.walked) {
            expanded.insert(Node::Volume(v.id));
        }
        Self {
            entries,
            volumes,
            by_id,
            children,
            expanded,
            marked: BTreeSet::new(),
            show_deleted: true,
            show_streams: false,
        }
    }

    pub fn entry(&self, id: u64) -> Option<&StoredEntry> {
        self.by_id.get(&id).and_then(|&i| self.entries.get(i))
    }

    fn visible(&self, e: &StoredEntry) -> bool {
        (self.show_deleted || e.entry.state == EntryState::Live)
            && (self.show_streams || e.entry.kind != EntryKind::Stream)
    }

    fn kids(&self, n: Node) -> impl Iterator<Item = &StoredEntry> {
        self.children
            .get(&n)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|&i| self.entries.get(i))
            .filter(|e| self.visible(e))
    }

    fn sub_dirs(&self, n: Node) -> Vec<Node> {
        let mut out: Vec<Node> = self
            .kids(n)
            .filter(|e| e.entry.kind == EntryKind::Directory)
            .map(|e| Node::Dir(e.id.0))
            .collect();
        if let Node::Volume(v) = n
            && self.children.get(&Node::Lost(v)).is_some_and(|c| !c.is_empty())
        {
            out.push(Node::Lost(v));
        }
        out
    }

    pub fn label(&self, n: Node) -> String {
        match n {
            Node::Volume(v) => self
                .volumes
                .iter()
                .find(|x| x.id == v)
                .map(|x| {
                    let c = &x.candidate;
                    let name = c.label.clone().unwrap_or_else(|| c.fs.as_str().to_uppercase());
                    format!("{name} · #{v}")
                })
                .unwrap_or_else(|| format!("#{v}")),
            Node::Lost(_) => LOST.to_owned(),
            Node::Dir(id) => self.entry(id).map(|e| e.entry.name.clone()).unwrap_or_default(),
        }
    }

    /// The visible tree rows: walked volumes, then expanded folders depth first.
    pub fn tree(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        for v in self.volumes.iter().filter(|v| v.walked) {
            self.push_rows(Node::Volume(v.id), 0, false, &mut rows);
        }
        rows
    }

    fn push_rows(&self, n: Node, depth: u16, lost: bool, rows: &mut Vec<TreeRow>) {
        if rows.len() > 200_000 || depth > 128 {
            return;
        }
        let subs = self.sub_dirs(n);
        let expanded = self.expanded.contains(&n);
        let lost = lost || matches!(n, Node::Lost(_));
        let deleted = matches!(n, Node::Dir(id) if self.entry(id).is_some_and(|e| e.entry.state == EntryState::Deleted));
        rows.push(TreeRow {
            node: n,
            depth,
            label: self.label(n),
            expandable: !subs.is_empty(),
            expanded,
            lost,
            deleted,
        });
        if expanded {
            for s in subs {
                self.push_rows(s, depth + 1, lost, rows);
            }
        }
    }

    /// Files directly in `n` (or every file matching `query` when searching).
    pub fn files(&self, n: Node, query: &str) -> Vec<&StoredEntry> {
        if !query.is_empty() {
            let q = query.to_lowercase();
            return self
                .entries
                .iter()
                .filter(|e| e.entry.kind != EntryKind::Directory && self.visible(e))
                .filter(|e| e.entry.name.to_lowercase().contains(&q))
                .take(100_000)
                .collect();
        }
        self.kids(n)
            .filter(|e| e.entry.kind != EntryKind::Directory)
            .collect()
    }

    /// Every file below `n`, for "recover this folder".
    pub fn files_below(&self, n: Node) -> Vec<u64> {
        let mut out = Vec::new();
        let mut stack = vec![n];
        let mut guard = 0u32;
        while let Some(x) = stack.pop() {
            guard += 1;
            if guard > 10_000_000 {
                break;
            }
            for e in self.kids(x) {
                if e.entry.kind == EntryKind::Directory {
                    stack.push(Node::Dir(e.id.0));
                } else {
                    out.push(e.id.0);
                }
            }
            if let Node::Volume(v) = x {
                stack.push(Node::Lost(v));
            }
        }
        out
    }

    pub fn toggle(&mut self, n: Node) {
        if !self.expanded.remove(&n) {
            self.expanded.insert(n);
        }
    }

    pub fn toggle_mark(&mut self, id: u64) {
        if !self.marked.remove(&id) {
            self.marked.insert(id);
        }
    }

    pub fn total_files(&self) -> (u64, u64) {
        self.entries
            .iter()
            .filter(|e| e.entry.kind == EntryKind::File)
            .fold((0, 0), |(n, b), e| (n + 1, b + e.entry.size))
    }

    pub fn folders(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.entry.kind == EntryKind::Directory)
            .count() as u64
    }
}

/// How a file's bytes were found, as the design system's chip: label and kind
/// (0 neutral, 1 mirror FAT, 2 a guess, 3 carved).
pub fn assembly_chip(a: rescyo_fs::AssemblyMethod) -> (&'static str, u8) {
    use rescyo_fs::AssemblyMethod as A;
    match a {
        A::FatChain => ("FAT CHAIN", 0),
        A::FatChainMirror => ("MIRROR FAT", 1),
        A::ExfatBitmapContiguous => ("EXFAT", 0),
        A::MftRunlist => ("MFT", 0),
        A::Ext4Extents | A::HfsExtents => ("EXTENTS", 0),
        A::BlockMap => ("BLOCK MAP", 0),
        A::ContiguousAssumed => ("CONTIGUOUS?", 2),
        A::Carved => ("CARVED", 3),
    }
}

/// The Bad-sector map of a `size`-byte Source as `n` strip cells: 0 not read, 1 readable,
/// 2 unreadable; `read_until` marks how far a running scan has come.
pub fn strip_cells(size: u64, bad: &[std::ops::Range<u64>], read_until: Option<u64>, n: u64) -> Vec<i32> {
    let size = size.max(1);
    (0..n)
        .map(|i| {
            let a = i * size / n;
            let b = ((i + 1) * size / n).max(a + 1);
            if bad.iter().any(|r| r.start < b && a < r.end) {
                2
            } else if read_until.is_none_or(|h| a < h) {
                1
            } else {
                0
            }
        })
        .collect()
}
