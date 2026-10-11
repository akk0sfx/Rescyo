//! Tree reconstruction from MFT records.
//!
//! The tree is rebuilt from the parent references in FILE_NAME attributes, not from directory
//! indexes: it survives a destroyed root directory or damaged `$I30` indexes, and deleted files
//! keep their parent reference.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use rescyo_fs::{
    AssemblyMethod, CancelToken, Codec, DataLayout, Entry, EntryId, EntryKind, EntryState, Extent, Limits,
    Note, Parent, Phase, ScanSink, SeedOrigin, SinkError, Times,
};
use rescyo_io::{GuardError, GuardedReader};

use crate::boot::NtfsGeometry;
use crate::record::{
    self, ATTR_DATA, ATTR_FILE_NAME, ATTR_REPARSE_POINT, ATTR_STANDARD_INFORMATION, AttrBody,
};
use crate::scan::MftScan;

const ROOT: u64 = 5;
const EXTEND: u64 = 11;
const FIRST_USER_RECORD: u64 = 16;
/// Consecutive non-records after which a contiguous MFT is assumed to end.
const MFT_END_GAP: u64 = 4096;
const CHUNK: u64 = 1 << 20;
/// The named stream holding a WOF-compressed file's content.
const WOF_STREAM: &str = "WofCompressedData";
/// Named streams kept per file (more is hostile).
const MAX_STREAMS: usize = 256;

/// One data attribute as gathered from a record and its extensions.
#[derive(Debug, Clone, Default)]
struct RawStream {
    resident: Option<Vec<u8>>,
    segments: Vec<(u64, Vec<record::Run>)>,
    data_size: u64,
    init_size: u64,
    /// Compression method flags and log2 clusters per unit.
    compression: (u16, u8),
    encrypted: bool,
    broken: bool,
}

#[derive(Debug, Default)]
struct Streams {
    main: RawStream,
    named: BTreeMap<String, RawStream>,
    reparse: Option<Vec<u8>>,
}

fn wof_codec(algorithm: u32) -> Option<Codec> {
    Some(match algorithm {
        0 => Codec::WofXpress4k,
        1 => Codec::WofLzx,
        2 => Codec::WofXpress8k,
        3 => Codec::WofXpress16k,
        _ => return None,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkStats {
    pub records: u64,
    pub files: u64,
    pub dirs: u64,
    pub deleted: u64,
    pub lost_folders: u64,
    /// Where the MFT layout came from: `mft`, `mftmirr`, `scan` or `contiguous`.
    pub mft_source: &'static str,
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

/// How record numbers map to Source offsets.
#[derive(Debug)]
enum Locator {
    /// The MFT's own runlist: (first record of the run, absolute offset, records in the run).
    Runs(Vec<(u64, u64, u64)>),
    /// Record numbers found by a scan.
    Map(BTreeMap<u64, u64>),
    /// Assumed contiguous from the recorded MFT offset.
    Contiguous { start: u64, count: u64 },
}

impl Locator {
    fn offset(&self, n: u64, rs: u64) -> Option<u64> {
        match self {
            Self::Runs(runs) => runs
                .iter()
                .find(|r| n >= r.0 && n < r.0 + r.2)
                .map(|r| r.1 + (n - r.0) * rs),
            Self::Map(m) => m.get(&n).copied(),
            Self::Contiguous { start, count } => (n < *count).then(|| start + n * rs),
        }
    }

    fn count(&self) -> u64 {
        match self {
            Self::Runs(runs) => runs.iter().map(|r| r.0 + r.2).max().unwrap_or(0),
            Self::Map(m) => m.keys().next_back().map_or(0, |k| k + 1),
            Self::Contiguous { count, .. } => *count,
        }
    }
}

/// What the first pass keeps per base record.
#[derive(Debug, Clone)]
struct Info {
    seq: u16,
    in_use: bool,
    is_dir: bool,
    name: String,
    parent: u64,
    parent_seq: u16,
    times: Times,
}

/// Reconstructs the tree of one NTFS volume into `sink`. `scan` (from a deep scan) is used when
/// both `$MFT` and `$MFTMirr` are unreadable.
pub fn walk(
    r: &GuardedReader,
    geo: &NtfsGeometry,
    scan: Option<&MftScan>,
    limits: &Limits,
    sink: &mut dyn ScanSink,
    cancel: &CancelToken,
) -> Result<WalkStats, WalkError> {
    if !geo.is_sane() {
        return Err(WalkError::InvalidGeometry);
    }
    let rs = u64::from(geo.record_size);
    let mut stats = WalkStats::default();
    let locator = locate_mft(r, geo, scan, &mut stats, sink)?;
    let count = locator.count().min(u64::from(u32::MAX));

    // Pass 1: names, parents, flags; extension records by base.
    let mut infos: HashMap<u64, Info> = HashMap::new();
    let mut extensions: HashMap<u64, Vec<u64>> = HashMap::new();
    for n in 0..count {
        if n % 4096 == 0 {
            if cancel.is_cancelled() {
                return Err(WalkError::Cancelled);
            }
            sink.progress(Phase::Walk, n, Some(count));
        }
        let Some(rec) = read_record(r, &locator, n, rs)? else {
            continue;
        };
        let Some(h) = record::header(&rec) else { continue };
        stats.records += 1;
        if h.base != 0 {
            extensions.entry(h.base).or_default().push(n);
            continue;
        }
        let attrs = record::attributes(&rec, &h);
        let mut best: Option<record::FileName> = None;
        let mut times = Times::default();
        for a in &attrs {
            match (a.kind, &a.body) {
                (ATTR_FILE_NAME, AttrBody::Resident(v)) => {
                    if let Some(f) = record::file_name(v) {
                        // Prefer Win32 (1) or Win32&DOS (3) names over DOS 8.3 (2).
                        let rank = |ns: u8| match ns {
                            1 | 3 => 0,
                            0 => 1,
                            _ => 2,
                        };
                        if best
                            .as_ref()
                            .is_none_or(|b| rank(f.namespace) < rank(b.namespace))
                        {
                            best = Some(f);
                        }
                    }
                }
                (ATTR_STANDARD_INFORMATION, AttrBody::Resident(v)) => times = record::standard_times(v),
                _ => {}
            }
        }
        if let Some(f) = best {
            infos.insert(
                n,
                Info {
                    seq: h.sequence,
                    in_use: h.in_use,
                    is_dir: h.is_dir,
                    name: f.name,
                    parent: f.parent & 0x0000_FFFF_FFFF_FFFF,
                    parent_seq: u16::try_from(f.parent >> 48).unwrap_or(0),
                    times,
                },
            );
        }
    }

    // Children by parent; a parent counts only if its sequence matches (or it was freed right
    // after, which bumps the sequence once).
    let parent_ok = |info: &Info, infos: &HashMap<u64, Info>| -> bool {
        if info.parent == ROOT {
            return true;
        }
        infos.get(&info.parent).is_some_and(|p| {
            p.is_dir && (p.seq == info.parent_seq || (!p.in_use && p.seq == info.parent_seq.wrapping_add(1)))
        })
    };
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut user: Vec<u64> = infos
        .keys()
        .copied()
        .filter(|&n| n >= FIRST_USER_RECORD)
        .collect();
    user.sort_unstable();
    for &n in &user {
        let Some(info) = infos.get(&n) else { continue };
        if info.parent == EXTEND {
            continue;
        }
        if parent_ok(info, &infos) {
            children.entry(info.parent).or_default().push(n);
        }
    }

    let mut w = Emitter {
        r,
        locator,
        geo,
        limits,
        sink,
        stats,
        emitted: 0,
        extensions,
        done: HashSet::new(),
    };
    w.subtree(&infos, &children, ROOT, Parent::Root, cancel)?;

    // Records whose parent chain does not reach the root: hang the highest one as lost.
    for &n in &user {
        if w.done.contains(&n) {
            continue;
        }
        let Some(info) = infos.get(&n) else { continue };
        if info.parent == EXTEND || parent_ok(info, &infos) && w.done.contains(&info.parent) {
            continue;
        }
        let mut top = n;
        for _ in 0..1024 {
            let Some(t) = infos.get(&top) else { break };
            if parent_ok(t, &infos) && t.parent != ROOT && !w.done.contains(&t.parent) && t.parent != top {
                top = t.parent;
            } else {
                break;
            }
        }
        if w.done.contains(&top) {
            continue;
        }
        let Some(t) = infos.get(&top) else { continue };
        let parent = Parent::Lost { parent_ref: t.parent };
        w.stats.lost_folders += u64::from(t.is_dir);
        w.node(&infos, &children, top, parent, cancel)?;
    }
    if w.stats.lost_folders > 0 {
        let count = w.stats.lost_folders;
        w.sink.note(Note::LostFolders { count });
    }
    Ok(w.stats)
}

struct Emitter<'a> {
    r: &'a GuardedReader,
    locator: Locator,
    geo: &'a NtfsGeometry,
    limits: &'a Limits,
    sink: &'a mut dyn ScanSink,
    stats: WalkStats,
    emitted: u64,
    extensions: HashMap<u64, Vec<u64>>,
    done: HashSet<u64>,
}

impl Emitter<'_> {
    /// Emits the children of `dir` under `parent`, breadth first.
    fn subtree(
        &mut self,
        infos: &HashMap<u64, Info>,
        children: &HashMap<u64, Vec<u64>>,
        dir: u64,
        parent: Parent,
        cancel: &CancelToken,
    ) -> Result<(), WalkError> {
        let mut queue: VecDeque<(u64, Parent, u32)> = VecDeque::from([(dir, parent, 0)]);
        self.done.insert(dir);
        while let Some((d, p, depth)) = queue.pop_front() {
            if cancel.is_cancelled() {
                return Err(WalkError::Cancelled);
            }
            if depth > self.limits.max_depth {
                self.sink.note(Note::LimitReached {
                    what: "directory depth".into(),
                });
                continue;
            }
            for &c in children.get(&d).map(Vec::as_slice).unwrap_or_default() {
                if !self.done.insert(c) {
                    continue;
                }
                let Some(info) = infos.get(&c) else { continue };
                if let Some(id) = self.emit(c, info, p)?
                    && info.is_dir
                {
                    queue.push_back((c, Parent::Entry(id), depth + 1));
                }
            }
        }
        Ok(())
    }

    /// Emits one record and, if it is a directory, everything below it.
    fn node(
        &mut self,
        infos: &HashMap<u64, Info>,
        children: &HashMap<u64, Vec<u64>>,
        n: u64,
        parent: Parent,
        cancel: &CancelToken,
    ) -> Result<(), WalkError> {
        if !self.done.insert(n) {
            return Ok(());
        }
        let Some(info) = infos.get(&n) else { return Ok(()) };
        if let Some(id) = self.emit(n, info, parent)?
            && info.is_dir
        {
            self.done.remove(&n);
            self.subtree(infos, children, n, Parent::Entry(id), cancel)?;
        }
        Ok(())
    }

    fn emit(&mut self, n: u64, info: &Info, parent: Parent) -> Result<Option<EntryId>, WalkError> {
        if self.emitted >= self.limits.max_entries {
            return Ok(None);
        }
        self.emitted += 1;
        let state = if info.in_use {
            EntryState::Live
        } else {
            EntryState::Deleted
        };
        if state == EntryState::Deleted {
            self.stats.deleted += 1;
        }
        let entry = |kind, name: String, size, layout| Entry {
            parent,
            name,
            name_raw: None,
            kind,
            state,
            size,
            times: info.times,
            layout,
            assembly: AssemblyMethod::MftRunlist,
            fs_ref: n,
            seed: (kind == EntryKind::Directory).then_some(SeedOrigin::Walk),
        };
        if info.is_dir {
            self.stats.dirs += 1;
            let e = entry(EntryKind::Directory, info.name.clone(), 0, DataLayout::default());
            return Ok(Some(self.sink.entry(e)?));
        }
        self.stats.files += 1;
        let mut streams = self.streams(n)?;
        let main = std::mem::take(&mut streams.main);
        let (mut size, mut layout) = self.layout_of(n, &main);
        match streams.reparse.as_deref().and_then(record::wof_algorithm) {
            Some(Ok(alg)) => match (wof_codec(alg), streams.named.remove(WOF_STREAM)) {
                (Some(codec), Some(w)) => {
                    let (_, stored) = self.layout_of(n, &w);
                    size = main.data_size;
                    layout = DataLayout {
                        extents: vec![Extent::Encoded {
                            codec,
                            stored: stored.extents,
                            len: size,
                        }],
                    };
                }
                _ => self.sink.note(Note::LimitReached {
                    what: format!("record {n}: WOF compression {alg} without readable data"),
                }),
            },
            Some(Err(provider)) => self.sink.note(Note::LimitReached {
                what: format!("record {n}: content kept outside the volume (WOF provider {provider})"),
            }),
            None => {}
        }
        let id = self
            .sink
            .entry(entry(EntryKind::File, info.name.clone(), size, layout))?;
        for (name, raw) in std::mem::take(&mut streams.named) {
            if self.emitted >= self.limits.max_entries {
                break;
            }
            self.emitted += 1;
            let (size, layout) = self.layout_of(n, &raw);
            let name = format!("{}:{name}", info.name);
            self.sink.entry(entry(EntryKind::Stream, name, size, layout))?;
        }
        Ok(Some(id))
    }

    /// The data streams and reparse point of record `n`, from it and its extension records.
    fn streams(&mut self, n: u64) -> Result<Streams, WalkError> {
        let rs = u64::from(self.geo.record_size);
        let mut records = vec![n];
        records.extend(self.extensions.get(&n).cloned().unwrap_or_default());
        let max_clusters = self.geo.size / u64::from(self.geo.cluster_size) + 1;
        let max_runs = usize::try_from(self.limits.max_extents).unwrap_or(usize::MAX);
        let mut out = Streams::default();
        for rn in records {
            let Some(off) = self.locator_offset(rn, rs) else {
                continue;
            };
            let Some(rec) = read_at(self.r, off, rs)? else {
                continue;
            };
            let Some(h) = record::header(&rec) else { continue };
            for a in record::attributes(&rec, &h) {
                if a.kind == ATTR_REPARSE_POINT {
                    if let AttrBody::Resident(v) = a.body {
                        out.reparse = Some(v.to_vec());
                    }
                    continue;
                }
                if a.kind != ATTR_DATA {
                    continue;
                }
                let st = if a.name.is_empty() {
                    &mut out.main
                } else {
                    if !out.named.contains_key(&a.name) && out.named.len() >= MAX_STREAMS {
                        continue;
                    }
                    out.named.entry(a.name.clone()).or_default()
                };
                st.encrypted |= a.flags & record::FLAG_ENCRYPTED != 0;
                match a.body {
                    AttrBody::Resident(v) => st.resident = Some(v.to_vec()),
                    AttrBody::NonResident {
                        start_vcn,
                        runs,
                        data_size,
                        init_size,
                        compression_unit,
                        ..
                    } => {
                        if start_vcn == 0 {
                            st.data_size = data_size;
                            st.init_size = init_size.min(data_size);
                            st.compression = (a.flags & record::FLAG_COMPRESSION_MASK, compression_unit);
                        }
                        if let Some(r) = record::decode_runs(runs, max_runs, max_clusters) {
                            st.segments.push((start_vcn, r));
                        } else {
                            st.broken = true;
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// The size and layout of one stream.
    fn layout_of(&mut self, n: u64, st: &RawStream) -> (u64, DataLayout) {
        if st.broken {
            self.sink.note(Note::ChainCut { cluster: n });
        }
        if st.encrypted {
            self.sink.note(Note::LimitReached {
                what: format!("record {n}: encrypted (EFS) data cannot be read without the user's key"),
            });
            return (st.data_size, DataLayout::default());
        }
        if let Some(v) = &st.resident {
            return (
                v.len() as u64,
                DataLayout {
                    extents: vec![Extent::Inline { data: v.clone() }],
                },
            );
        }
        let mut segments = st.segments.clone();
        segments.sort_by_key(|s| s.0);
        let runs: Vec<record::Run> = segments.into_iter().flat_map(|s| s.1).collect();
        match st.compression {
            (0, _) => {}
            (record::FLAG_COMPRESSED, unit @ 1..=8) => {
                return (st.data_size, self.units(n, &runs, unit, st.data_size));
            }
            (method, _) => {
                self.sink.note(Note::LimitReached {
                    what: format!("record {n}: unknown compression method {method}"),
                });
                return (st.data_size, DataLayout::default());
            }
        }
        let c = u64::from(self.geo.cluster_size);
        let vol_end = self.geo.volume_offset + self.geo.size;
        let mut layout = DataLayout::default();
        let mut left = st.init_size;
        for run in runs {
            if left == 0 {
                break;
            }
            let len = (run.clusters * c).min(left);
            match run.lcn.and_then(|l| self.geo.lcn_offset(l)) {
                Some(off) if off + len <= vol_end => layout.push_disk(off, len),
                Some(_) => {
                    self.sink.note(Note::ChainCut { cluster: n });
                    break;
                }
                None => layout.extents.push(Extent::Zero { len }),
            }
            left -= len;
        }
        // Bytes past the initialized size read as zeros.
        if layout.len() == st.init_size && st.data_size > st.init_size {
            layout.extents.push(Extent::Zero {
                len: st.data_size - st.init_size,
            });
        }
        (st.data_size, layout)
    }

    /// A compressed stream, one compression unit at a time: fully allocated units are stored
    /// as they are, empty ones are zeros, and partly allocated ones hold LZNT1 data.
    fn units(&mut self, n: u64, runs: &[record::Run], unit: u8, size: u64) -> DataLayout {
        let c = u64::from(self.geo.cluster_size);
        let per_unit = 1u64 << unit;
        let unit_bytes = per_unit * c;
        let vol_end = self.geo.volume_offset + self.geo.size;
        let mut layout = DataLayout::default();
        let (mut ri, mut used) = (0usize, 0u64);
        let mut pos = 0u64;
        while pos < size {
            if layout.extents.len() as u64 >= u64::from(self.limits.max_extents) {
                self.sink.note(Note::LimitReached {
                    what: format!("record {n}: compression units"),
                });
                break;
            }
            // The clusters of this unit: (absolute offset or sparse, clusters).
            let mut pieces: Vec<(Option<u64>, u64)> = Vec::new();
            let mut need = per_unit;
            while need > 0 {
                let Some(run) = runs.get(ri) else {
                    pieces.push((None, need));
                    break;
                };
                let take = (run.clusters - used).min(need);
                let off = run.lcn.and_then(|l| self.geo.lcn_offset(l + used));
                if run.lcn.is_some() && off.is_none_or(|o| o + take * c > vol_end) {
                    self.sink.note(Note::ChainCut { cluster: n });
                    return layout;
                }
                pieces.push((off, take));
                need -= take;
                used += take;
                if used == run.clusters {
                    ri += 1;
                    used = 0;
                }
            }
            let len = unit_bytes.min(size - pos);
            let allocated: u64 = pieces.iter().filter(|p| p.0.is_some()).map(|p| p.1).sum();
            if allocated == 0 {
                layout.extents.push(Extent::Zero { len });
            } else if allocated == per_unit {
                let mut left = len;
                for (off, k) in pieces {
                    let l = (k * c).min(left);
                    layout.push_disk(off.unwrap_or(0), l);
                    left -= l;
                }
            } else {
                let mut stored = DataLayout::default();
                for (off, k) in pieces {
                    if let Some(o) = off {
                        stored.push_disk(o, k * c);
                    }
                }
                layout.extents.push(Extent::Encoded {
                    codec: Codec::Lznt1,
                    stored: stored.extents,
                    len,
                });
            }
            pos += len;
        }
        layout
    }

    fn locator_offset(&self, n: u64, rs: u64) -> Option<u64> {
        self.locator.offset(n, rs)
    }
}

fn read_at(r: &GuardedReader, off: u64, rs: u64) -> Result<Option<Vec<u8>>, WalkError> {
    let mut rec = vec![0u8; usize::try_from(rs).map_err(|_| WalkError::InvalidGeometry)?];
    match r.read(off, &mut rec) {
        Ok(rep) if rep.is_clean() => {}
        Ok(_) | Err(GuardError::OutOfBounds) => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    Ok(record::apply_fixup(&mut rec, 512).then_some(rec))
}

fn read_record(r: &GuardedReader, loc: &Locator, n: u64, rs: u64) -> Result<Option<Vec<u8>>, WalkError> {
    match loc.offset(n, rs) {
        Some(off) => read_at(r, off, rs),
        None => Ok(None),
    }
}

/// Finds the MFT layout: `$MFT`'s own runlist, then `$MFTMirr`'s copy of record 0, then the
/// scan's record map, then a contiguous MFT from the recorded offset.
fn locate_mft(
    r: &GuardedReader,
    geo: &NtfsGeometry,
    scan: Option<&MftScan>,
    stats: &mut WalkStats,
    sink: &mut dyn ScanSink,
) -> Result<Locator, WalkError> {
    let rs = u64::from(geo.record_size);
    let c = u64::from(geo.cluster_size);
    let candidates = [(Some(geo.mft_offset), "mft"), (geo.mftmirr_offset, "mftmirr")];
    for (off, source) in candidates {
        let Some(off) = off else { continue };
        let Some(rec) = read_at(r, off, rs)? else {
            continue;
        };
        let Some(h) = record::header(&rec) else { continue };
        if h.number.is_some_and(|n| n != 0) || h.base != 0 {
            continue;
        }
        let max_clusters = geo.size / c + 1;
        for a in record::attributes(&rec, &h) {
            if a.kind != ATTR_DATA || !a.name.is_empty() {
                continue;
            }
            let AttrBody::NonResident { runs, data_size, .. } = a.body else {
                continue;
            };
            let Some(runs) = record::decode_runs(runs, 65_536, max_clusters) else {
                continue;
            };
            let mut out = Vec::new();
            let mut first = 0u64;
            let total = data_size / rs;
            for run in runs {
                let Some(lcn) = run.lcn else { break };
                let Some(abs) = geo.lcn_offset(lcn) else { break };
                let n = (run.clusters * c / rs).min(total.saturating_sub(first));
                out.push((first, abs, n));
                first += n;
            }
            if !out.is_empty() {
                if source == "mftmirr" {
                    sink.note(Note::BackupBootSectorUsed { offset: off });
                }
                stats.mft_source = source;
                return Ok(Locator::Runs(out));
            }
        }
    }
    if let Some(s) = scan {
        let lo = geo.volume_offset;
        let hi = geo.volume_offset + geo.size;
        let map: BTreeMap<u64, u64> = s
            .records
            .iter()
            .filter(|h| h.offset >= lo && h.offset < hi && h.alloc == geo.record_size)
            .filter_map(|h| h.number.map(|n| (u64::from(n), h.offset)))
            .collect();
        if !map.is_empty() {
            sink.note(Note::GeometryInferred);
            stats.mft_source = "scan";
            return Ok(Locator::Map(map));
        }
    }
    // Last resort: a contiguous MFT, ending after a long run of non-records.
    let mut count = 0;
    let mut gap = 0;
    let mut n = 0u64;
    while gap < MFT_END_GAP {
        let off = geo.mft_offset + n * rs;
        if off + rs > geo.volume_offset + geo.size {
            break;
        }
        let mut buf = vec![0u8; usize::try_from(CHUNK.min(rs)).unwrap_or(1024)];
        let ok = r.read(off, &mut buf).is_ok_and(|rep| rep.is_clean()) && buf.starts_with(b"FILE");
        if ok {
            count = n + 1;
            gap = 0;
        } else {
            gap += 1;
        }
        n += 1;
    }
    sink.note(Note::GeometryInferred);
    stats.mft_source = "contiguous";
    Ok(Locator::Contiguous {
        start: geo.mft_offset,
        count,
    })
}
