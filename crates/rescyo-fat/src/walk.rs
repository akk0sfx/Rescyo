//! Tree reconstruction: from the root and from directory seeds to Entries with data layouts.

use std::collections::{BTreeMap, HashMap, HashSet};

use rescyo_fs::codepage::{self, Codepage};
use rescyo_fs::{
    AssemblyMethod, CancelToken, DataLayout, DirectorySeed, Entry, EntryId, EntryKind, EntryState, Extent,
    Limits, Note, Parent, Phase, ScanSink, SeedOrigin, SinkError, Times,
};
use rescyo_io::{GuardError, GuardedReader};

use crate::dir::{self, DirContext};
use crate::exdir::{self, ExContext};
use crate::geometry::{FatGeometry, RootDir};
use crate::table::{FatTable, Link};

/// FAT directories hold at most 65 536 entries.
const FAT_DIR_MAX_BYTES: u64 = 65_536 * 32;
/// Directories sampled to guess the code page of short names.
const CODEPAGE_SAMPLE_DIRS: usize = 64;

/// What to walk and how.
#[derive(Debug, Clone, Default)]
pub struct WalkOptions {
    /// Code page for short names; detected when `None`.
    pub codepage: Option<Codepage>,
    /// Tie-breaker for detection (the user's locale).
    pub locale_codepage: Option<Codepage>,
    /// Directory starts found by scans or hints; unreached ones become Lost folders.
    pub seeds: Vec<DirectorySeed>,
}

/// Counts reported after a walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkStats {
    pub files: u64,
    pub dirs: u64,
    pub deleted: u64,
    pub guessed: u64,
    pub lost_folders: u64,
    pub seeds_rejected: u64,
    pub codepage: Option<Codepage>,
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
}

/// Where a directory's bytes come from.
#[derive(Debug, Clone, Copy)]
enum DirSource {
    Region {
        offset: u64,
        len: u64,
    },
    Chain {
        first: u32,
        known_len: Option<u64>,
        contiguous: bool,
    },
}

#[derive(Debug)]
struct Job {
    source: DirSource,
    parent: Parent,
    depth: u32,
    deleted: bool,
}

/// Reconstructs the tree of one FAT or exFAT volume into `sink`.
pub fn walk(
    r: &GuardedReader,
    geo: &FatGeometry,
    opts: &WalkOptions,
    limits: &Limits,
    sink: &mut dyn ScanSink,
    cancel: &CancelToken,
) -> Result<WalkStats, WalkError> {
    if !geo.is_sane() {
        return Err(WalkError::InvalidGeometry);
    }
    let mut w = Walker::new(r, geo, limits, sink, cancel);
    let cp = match opts.codepage {
        Some(cp) => {
            w.sink.note(Note::Codepage {
                name: cp.name().into(),
                detected: false,
            });
            cp
        }
        None if geo.is_exfat() => Codepage::Cp437,
        None => {
            let cp = w.detect_codepage(opts.locale_codepage)?;
            w.sink.note(Note::Codepage {
                name: cp.name().into(),
                detected: true,
            });
            cp
        }
    };
    w.ctx.codepage = cp;
    if geo.is_exfat() {
        w.ex_seeds = opts
            .seeds
            .iter()
            .filter_map(|s| u32::try_from(s.cluster).ok())
            .take(65_536)
            .collect();
    }
    w.stats.codepage = Some(cp);

    match geo.root {
        RootDir::Region { offset, len } => {
            w.push(DirSource::Region { offset, len }, Parent::Root, 0, false);
        }
        RootDir::Cluster { cluster } => {
            w.visited.insert(cluster);
            w.push(
                DirSource::Chain {
                    first: cluster,
                    known_len: None,
                    contiguous: false,
                },
                Parent::Root,
                0,
                false,
            );
        }
        RootDir::Unknown => {}
    }
    w.sink.progress(Phase::Walk, 0, None);
    w.run()?;
    w.lost_folders(&opts.seeds)?;
    Ok(w.stats)
}

struct Walker<'a> {
    r: &'a GuardedReader,
    geo: &'a FatGeometry,
    fats: Vec<FatTable>,
    limits: &'a Limits,
    sink: &'a mut dyn ScanSink,
    cancel: &'a CancelToken,
    ctx: DirContext,
    ex: ExContext,
    jobs: Vec<Job>,
    /// First clusters of directories already walked or scheduled.
    visited: HashSet<u32>,
    /// Every cluster read as part of some directory (first and continuation clusters).
    dir_clusters: HashSet<u32>,
    /// exFAT directory clusters found by a scan: candidates for stitching split entry sets.
    ex_seeds: Vec<u32>,
    /// Entry ids of walked directories by first cluster.
    dir_ids: HashMap<u32, EntryId>,
    /// Names seen for directory clusters that were not walked from their entry.
    names: HashMap<u32, String>,
    emitted: u64,
    stats: WalkStats,
}

impl<'a> Walker<'a> {
    fn new(
        r: &'a GuardedReader,
        geo: &'a FatGeometry,
        limits: &'a Limits,
        sink: &'a mut dyn ScanSink,
        cancel: &'a CancelToken,
    ) -> Self {
        let fats = geo
            .fats
            .iter()
            .map(|&f| FatTable::new(f, geo.fs, geo.max_cluster))
            .collect();
        let max_size = geo.data_end().saturating_sub(geo.data_offset);
        Self {
            r,
            geo,
            fats,
            limits,
            sink,
            cancel,
            ctx: DirContext {
                fat32: geo.fs == rescyo_fs::FsKind::Fat32,
                max_cluster: geo.max_cluster,
                max_size,
                codepage: Codepage::Cp437,
            },
            ex: ExContext {
                max_cluster: geo.max_cluster,
                cluster_size: geo.cluster_size,
                max_size,
            },
            jobs: Vec::new(),
            visited: HashSet::new(),
            dir_clusters: HashSet::new(),
            ex_seeds: Vec::new(),
            dir_ids: HashMap::new(),
            names: HashMap::new(),
            emitted: 0,
            stats: WalkStats::default(),
        }
    }

    fn push(&mut self, source: DirSource, parent: Parent, depth: u32, deleted: bool) {
        self.jobs.push(Job {
            source,
            parent,
            depth,
            deleted,
        });
    }

    fn csize(&self) -> u64 {
        u64::from(self.geo.cluster_size)
    }

    fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>, WalkError> {
        let mut buf = vec![0u8; usize::try_from(len).map_err(|_| WalkError::InvalidGeometry)?];
        match self.r.read(offset, &mut buf) {
            Ok(_) | Err(GuardError::OutOfBounds) => Ok(buf),
            Err(e) => Err(e.into()),
        }
    }

    fn read_cluster(&self, c: u32) -> Result<Option<Vec<u8>>, WalkError> {
        match self.geo.cluster_offset(c) {
            Some(off) => Ok(Some(self.read(off, self.csize())?)),
            None => Ok(None),
        }
    }

    fn looks_like_dir(&self, d: &[u8], strict: bool) -> bool {
        if self.geo.is_exfat() {
            exdir::looks_like_exdir(d, &self.ex, strict)
        } else {
            dir::dir_start(d, self.ctx.fat32).is_none() && dir::looks_like_dir(d, &self.ctx, strict)
        }
    }

    fn link(&mut self, table: usize, c: u32) -> Result<Link, WalkError> {
        match self.fats.get_mut(table) {
            Some(t) => Ok(t.link(self.r, c)?),
            None => Ok(Link::Unknown),
        }
    }

    /// Reads a directory: a fixed region, a known contiguous run, or a cluster chain that falls
    /// back to the mirror FAT and finally to the next cluster when it still looks like a
    /// directory.
    fn dir_bytes(&mut self, source: DirSource) -> Result<Vec<u8>, WalkError> {
        let cap = if self.geo.is_exfat() {
            self.limits.max_dir_bytes
        } else {
            FAT_DIR_MAX_BYTES.min(self.limits.max_dir_bytes)
        };
        let max_clusters = (cap / self.csize()).max(1);
        match source {
            DirSource::Region { offset, len } => self.read(offset, len.min(cap)),
            DirSource::Chain {
                first,
                known_len: Some(len),
                contiguous: true,
            } => {
                let n = len.div_ceil(self.csize()).clamp(1, max_clusters);
                let last = u64::from(first) + n - 1;
                if last > u64::from(self.geo.max_cluster) {
                    return Ok(Vec::new());
                }
                let Some(off) = self.geo.cluster_offset(first) else {
                    return Ok(Vec::new());
                };
                for k in 0..n {
                    self.dir_clusters.insert(first + u32::try_from(k).unwrap_or(0));
                }
                self.read(off, n * self.csize())
            }
            DirSource::Chain { first, known_len, .. } => {
                let Some(mut data) = self.read_cluster(first)? else {
                    return Ok(Vec::new());
                };
                let limit =
                    known_len.map_or(max_clusters, |l| l.div_ceil(self.csize()).clamp(1, max_clusters));
                let mut seen = HashSet::from([first]);
                let mut cur = first;
                let mut cur_data_tail_used = data
                    .as_chunks::<32>()
                    .0
                    .last()
                    .is_some_and(|s| s.first() != Some(&0));
                for _ in 1..limit {
                    let mut next = None;
                    let mut ended = false;
                    for t in 0..self.fats.len() {
                        match self.link(t, cur)? {
                            Link::End => {
                                ended = true;
                                break;
                            }
                            Link::Next(v) if !seen.contains(&v) => {
                                if let Some(d) = self.read_cluster(v)?
                                    && self.looks_like_dir(&d, false)
                                {
                                    next = Some((v, d));
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    if ended && known_len.is_none() {
                        break;
                    }
                    // A split entry set verified by its checksum is stronger evidence than
                    // adjacency, so it is tried first.
                    if next.is_none()
                        && self.geo.is_exfat()
                        && let Some(t) = exdir::incomplete_tail(&data)
                    {
                        let tail = data.get(t..).unwrap_or_default().to_vec();
                        for s in self.ex_seeds.clone() {
                            if seen.contains(&s) || self.dir_clusters.contains(&s) {
                                continue;
                            }
                            if let Some(d) = self.read_cluster(s)?
                                && exdir::completes_set(&tail, &d, &self.ex)
                            {
                                self.sink.note(Note::DirectoryStitched {
                                    cluster: u64::from(s),
                                });
                                next = Some((s, d));
                                break;
                            }
                        }
                    }
                    if next.is_none() && (cur_data_tail_used || known_len.is_some()) {
                        let v = cur + 1;
                        // Never run into the first cluster of a directory known to be elsewhere.
                        if !seen.contains(&v)
                            && !self.visited.contains(&v)
                            && !self.dir_clusters.contains(&v)
                            && let Some(d) = self.read_cluster(v)?
                            && self.looks_like_dir(&d, true)
                        {
                            self.sink.note(Note::DirectoryContinuedHeuristically {
                                cluster: u64::from(v),
                            });
                            next = Some((v, d));
                        }
                    }
                    let Some((v, d)) = next else { break };
                    cur_data_tail_used = d
                        .as_chunks::<32>()
                        .0
                        .last()
                        .is_some_and(|s| s.first() != Some(&0));
                    data.extend_from_slice(&d);
                    seen.insert(v);
                    cur = v;
                }
                self.dir_clusters.extend(seen);
                Ok(data)
            }
        }
    }

    fn check(&self) -> Result<(), WalkError> {
        if self.cancel.is_cancelled() {
            Err(WalkError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn emit(&mut self, e: Entry) -> Result<Option<EntryId>, WalkError> {
        if self.emitted >= self.limits.max_entries {
            return Ok(None);
        }
        self.emitted += 1;
        match e.kind {
            EntryKind::File | EntryKind::Stream => self.stats.files += 1,
            EntryKind::Directory => self.stats.dirs += 1,
        }
        if e.state == EntryState::Deleted {
            self.stats.deleted += 1;
        }
        if e.assembly.is_guess() && e.kind == EntryKind::File && e.size > 0 {
            self.stats.guessed += 1;
        }
        Ok(Some(self.sink.entry(e)?))
    }

    fn run(&mut self) -> Result<(), WalkError> {
        while let Some(job) = self.jobs.pop() {
            self.check()?;
            if job.depth > self.limits.max_depth {
                self.sink.note(Note::LimitReached {
                    what: "directory depth".into(),
                });
                continue;
            }
            let bytes = self.dir_bytes(job.source)?;
            let max_entries = usize::try_from(self.limits.max_dir_entries).unwrap_or(usize::MAX);
            if self.geo.is_exfat() {
                let (entries, _) = exdir::parse_exdir(&bytes, &self.ex, max_entries);
                for e in entries {
                    self.ex_entry(e, &job)?;
                }
            } else {
                for e in dir::parse_dir(&bytes, &self.ctx, max_entries) {
                    self.fat_entry(e, &job)?;
                }
            }
            self.sink.progress(Phase::Walk, self.emitted, None);
        }
        Ok(())
    }

    fn fat_entry(&mut self, e: dir::RawEntry, job: &Job) -> Result<(), WalkError> {
        let deleted = e.deleted || job.deleted;
        let state = if deleted {
            EntryState::Deleted
        } else {
            EntryState::Live
        };
        if e.is_dir {
            if self.visited.contains(&e.cluster) {
                return Ok(());
            }
            // A deleted directory is walked only if its first cluster still starts it.
            if e.deleted {
                let Some(d) = self.read_cluster(e.cluster)? else {
                    return Ok(());
                };
                if dir::dir_start(&d, self.ctx.fat32).map(|(me, _)| me) != Some(e.cluster) {
                    self.names.entry(e.cluster).or_insert(e.name);
                    return Ok(());
                }
            }
            let entry = self.dir_entry(
                e.name,
                job.parent,
                state,
                e.times,
                e.cluster,
                Some(SeedOrigin::Walk),
            );
            if let Some(id) = self.emit(entry)? {
                self.visited.insert(e.cluster);
                self.dir_ids.insert(e.cluster, id);
                self.push(
                    DirSource::Chain {
                        first: e.cluster,
                        known_len: None,
                        contiguous: false,
                    },
                    Parent::Entry(id),
                    job.depth + 1,
                    deleted,
                );
            }
            return Ok(());
        }
        let size = u64::from(e.size);
        let (layout, assembly) = if deleted {
            (
                self.contiguous(e.cluster, size),
                AssemblyMethod::ContiguousAssumed,
            )
        } else {
            self.fat_layout(e.cluster, size)?
        };
        let raw = (!e.has_long_name).then(|| e.short.to_vec());
        self.emit(Entry {
            parent: job.parent,
            name: e.name,
            name_raw: raw,
            kind: EntryKind::File,
            state,
            size,
            times: e.times,
            layout,
            assembly,
            fs_ref: u64::from(e.cluster),
            seed: None,
        })?;
        Ok(())
    }

    fn ex_entry(&mut self, e: exdir::ExEntry, job: &Job) -> Result<(), WalkError> {
        let deleted = e.deleted || job.deleted;
        let state = if deleted {
            EntryState::Deleted
        } else {
            EntryState::Live
        };
        if e.is_dir {
            if e.first_cluster < 2 || self.visited.contains(&e.first_cluster) {
                return Ok(());
            }
            let entry = self.dir_entry(
                e.name,
                job.parent,
                state,
                e.times,
                e.first_cluster,
                Some(SeedOrigin::Walk),
            );
            if let Some(id) = self.emit(entry)? {
                self.visited.insert(e.first_cluster);
                self.dir_ids.insert(e.first_cluster, id);
                self.push(
                    DirSource::Chain {
                        first: e.first_cluster,
                        known_len: Some(e.data_len),
                        contiguous: e.no_fat_chain || deleted,
                    },
                    Parent::Entry(id),
                    job.depth + 1,
                    deleted,
                );
            }
            return Ok(());
        }
        let (mut layout, assembly) = if e.data_len == 0 {
            (DataLayout::default(), AssemblyMethod::ExfatBitmapContiguous)
        } else if e.no_fat_chain {
            (
                self.contiguous(e.first_cluster, e.valid_len),
                AssemblyMethod::ExfatBitmapContiguous,
            )
        } else if deleted {
            (
                self.contiguous(e.first_cluster, e.valid_len),
                AssemblyMethod::ContiguousAssumed,
            )
        } else {
            let (l, m) = self.fat_layout(e.first_cluster, e.data_len)?;
            (truncate(l, e.valid_len), m)
        };
        // Bytes past the valid data length are undefined on disk and read as zeros.
        if e.data_len > e.valid_len && layout.len() == e.valid_len {
            layout.extents.push(Extent::Zero {
                len: e.data_len - e.valid_len,
            });
        }
        self.emit(Entry {
            parent: job.parent,
            name: e.name,
            name_raw: None,
            kind: EntryKind::File,
            state,
            size: e.data_len,
            times: e.times,
            layout,
            assembly,
            fs_ref: u64::from(e.first_cluster),
            seed: None,
        })?;
        Ok(())
    }

    fn dir_entry(
        &self,
        name: String,
        parent: Parent,
        state: EntryState,
        times: Times,
        cluster: u32,
        seed: Option<SeedOrigin>,
    ) -> Entry {
        Entry {
            parent,
            name,
            name_raw: None,
            kind: EntryKind::Directory,
            state,
            size: 0,
            times,
            layout: DataLayout::default(),
            assembly: AssemblyMethod::FatChain,
            fs_ref: u64::from(cluster),
            seed,
        }
    }

    /// `size` bytes read as one run from `first`, clipped to the data area.
    fn contiguous(&self, first: u32, size: u64) -> DataLayout {
        let mut l = DataLayout::default();
        if let Some(off) = self.geo.cluster_offset(first) {
            let end = self.geo.data_end();
            l.push_disk(off, size.min(end.saturating_sub(off)));
        }
        l
    }

    /// Follows the chain of a live file through each FAT copy in turn; contiguous otherwise.
    fn fat_layout(&mut self, first: u32, size: u64) -> Result<(DataLayout, AssemblyMethod), WalkError> {
        if size == 0 {
            return Ok((DataLayout::default(), AssemblyMethod::FatChain));
        }
        if self.geo.cluster_offset(first).is_none() {
            return Ok((DataLayout::default(), AssemblyMethod::ContiguousAssumed));
        }
        let need = size.div_ceil(self.csize());
        for t in 0..self.fats.len() {
            if let Some(chain) = self.chain(t, first, need)? {
                let method = if t == 0 {
                    AssemblyMethod::FatChain
                } else {
                    AssemblyMethod::FatChainMirror
                };
                return Ok((self.layout_of(&chain, size), method));
            }
        }
        Ok((self.contiguous(first, size), AssemblyMethod::ContiguousAssumed))
    }

    /// Exactly `need` clusters ending in an end mark, without repeats; `None` if the table
    /// says anything else.
    fn chain(&mut self, t: usize, first: u32, need: u64) -> Result<Option<Vec<u32>>, WalkError> {
        if need > u64::from(self.geo.max_cluster) {
            return Ok(None);
        }
        let mut out = vec![first];
        let mut seen = HashSet::from([first]);
        let mut cur = first;
        while (out.len() as u64) < need {
            match self.link(t, cur)? {
                Link::Next(v) if seen.insert(v) => {
                    out.push(v);
                    cur = v;
                }
                _ => return Ok(None),
            }
        }
        Ok((self.link(t, cur)? == Link::End).then_some(out))
    }

    fn layout_of(&self, chain: &[u32], size: u64) -> DataLayout {
        let mut l = DataLayout::default();
        let mut left = size;
        for &c in chain {
            let Some(off) = self.geo.cluster_offset(c) else {
                break;
            };
            let n = left.min(self.csize());
            l.push_disk(off, n);
            left -= n;
            if left == 0 || l.extents.len() as u64 >= u64::from(self.limits.max_extents) {
                break;
            }
        }
        l
    }

    /// Samples short names reachable from the root and guesses their code page.
    fn detect_codepage(&mut self, prefer: Option<Codepage>) -> Result<Codepage, WalkError> {
        let mut shorts: Vec<[u8; 11]> = Vec::new();
        let mut queue: Vec<DirSource> = match self.geo.root {
            RootDir::Region { offset, len } => vec![DirSource::Region { offset, len }],
            RootDir::Cluster { cluster } => vec![DirSource::Chain {
                first: cluster,
                known_len: None,
                contiguous: false,
            }],
            RootDir::Unknown => Vec::new(),
        };
        let mut seen = HashSet::new();
        let mut dirs = 0;
        while let Some(src) = queue.pop() {
            if dirs >= CODEPAGE_SAMPLE_DIRS {
                break;
            }
            dirs += 1;
            let bytes = self.dir_bytes(src)?;
            for e in dir::parse_dir(&bytes, &self.ctx, 4096) {
                if !e.has_long_name && !e.deleted {
                    shorts.push(e.short);
                }
                if e.is_dir && !e.deleted && seen.insert(e.cluster) {
                    queue.push(DirSource::Chain {
                        first: e.cluster,
                        known_len: None,
                        contiguous: false,
                    });
                }
            }
        }
        Ok(codepage::detect(shorts.iter().map(|s| s.as_slice()), prefer)
            .or(prefer)
            .unwrap_or(Codepage::Cp437))
    }

    /// Walks seeds that the tree did not reach and hangs them where their parent is known, or
    /// under Lost folders.
    fn lost_folders(&mut self, seeds: &[DirectorySeed]) -> Result<(), WalkError> {
        // Parent cluster as recorded on disk; `None` where the format keeps no back link (exFAT).
        let mut parent_of: BTreeMap<u32, (Option<u32>, SeedOrigin)> = BTreeMap::new();
        let mut rejected = 0u64;
        for s in seeds {
            self.check()?;
            let Ok(c) = u32::try_from(s.cluster) else {
                rejected += 1;
                continue;
            };
            if self.visited.contains(&c) || self.dir_clusters.contains(&c) || parent_of.contains_key(&c) {
                continue;
            }
            let Some(d) = self.read_cluster(c)? else {
                rejected += 1;
                continue;
            };
            let parent = if self.geo.is_exfat() {
                exdir::looks_like_exdir(&d, &self.ex, true)
                    .then(|| s.parent.and_then(|p| u32::try_from(p).ok()))
            } else {
                // The cluster's own `..` is the truth; a hint's claimed parent is not. A seed
                // without a parent is a fragment: accepted only if it is entries throughout.
                match s.parent {
                    None => (dir::dir_start(&d, self.ctx.fat32).is_none()
                        && dir::score(&d, &self.ctx).1 == 0
                        && dir::looks_like_dir(&d, &self.ctx, true))
                    .then_some(None),
                    Some(_) => dir::dir_start(&d, self.ctx.fat32)
                        .filter(|(me, _)| *me == c)
                        .map(|(_, p)| Some(p)),
                }
            };
            match parent {
                Some(p) => {
                    parent_of.insert(c, (p, s.origin));
                }
                None => rejected += 1,
            }
        }
        if rejected > 0 {
            self.stats.seeds_rejected = rejected;
            self.sink.note(Note::SeedsRejected { count: rejected });
        }
        self.sink
            .progress(Phase::LostFolders, 0, Some(parent_of.len() as u64));
        let root_cluster = match self.geo.root {
            RootDir::Cluster { cluster } => Some(cluster),
            _ => None,
        };
        let candidates: Vec<u32> = parent_of.keys().copied().collect();
        for c in candidates {
            self.check()?;
            if self.visited.contains(&c) || self.dir_clusters.contains(&c) {
                continue;
            }
            // Climb to the highest unreached ancestor, so a whole lost subtree hangs once.
            let mut top = c;
            for _ in 0..1000 {
                match parent_of.get(&top) {
                    Some(&(Some(p), _))
                        if p != top && parent_of.contains_key(&p) && !self.visited.contains(&p) =>
                    {
                        top = p
                    }
                    _ => break,
                }
            }
            let Some(&(p, origin)) = parent_of.get(&top) else {
                continue;
            };
            let parent = match p {
                None => Parent::Lost { parent_ref: 0 },
                Some(p) if p == 0 || Some(p) == root_cluster => {
                    if matches!(self.geo.root, RootDir::Unknown) {
                        Parent::Lost { parent_ref: 0 }
                    } else {
                        Parent::Root
                    }
                }
                Some(p) => match self.dir_ids.get(&p) {
                    Some(&id) => Parent::Entry(id),
                    None => Parent::Lost {
                        parent_ref: u64::from(p),
                    },
                },
            };
            let name = self
                .names
                .get(&top)
                .cloned()
                .unwrap_or_else(|| format!("DIR_{top}"));
            let entry = self.dir_entry(
                name,
                parent,
                EntryState::Live,
                Times::default(),
                top,
                Some(origin),
            );
            if let Some(id) = self.emit(entry)? {
                if matches!(parent, Parent::Lost { .. }) {
                    self.stats.lost_folders += 1;
                }
                self.visited.insert(top);
                self.dir_ids.insert(top, id);
                let known_len = None;
                self.push(
                    DirSource::Chain {
                        first: top,
                        known_len,
                        contiguous: false,
                    },
                    Parent::Entry(id),
                    1,
                    false,
                );
                self.run()?;
            }
        }
        if self.stats.lost_folders > 0 {
            self.sink.note(Note::LostFolders {
                count: self.stats.lost_folders,
            });
        }
        Ok(())
    }
}

fn truncate(mut l: DataLayout, len: u64) -> DataLayout {
    let mut left = len;
    let mut out = DataLayout::default();
    for e in l.extents.drain(..) {
        if left == 0 {
            break;
        }
        match e {
            Extent::Disk { offset, len } => {
                let n = len.min(left);
                out.push_disk(offset, n);
                left -= n;
            }
            other => {
                left = left.saturating_sub(other.len());
                out.extents.push(other);
            }
        }
    }
    out
}
