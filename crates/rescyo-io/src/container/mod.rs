//! Virtual-disk containers (VHD, VHDX, VMDK, qcow2, EWF/E01, Apple DMG) as read-only Sources.
//!
//! A container maps the bytes of a virtual disk onto one or more host files through tables
//! (block allocation tables, grain tables, L1/L2 tables, chunk tables). Each format here only
//! describes that mapping as a [`Layout`]; [`ContainerSource`] does the reading, the zero
//! filling of unallocated areas, the reading through parent images and the decoding of
//! compressed chunks.
//!
//! Damage inside a container is local, as on a disk: a table entry that points outside its file,
//! a chunk that fails its checksum or does not decompress makes only that range unreadable
//! ([`ReadError::Media`]), so the [`crate::GuardedReader`] zero-fills it and records it in the
//! Bad-sector map. Only damage to the container's own header, without a usable copy, stops the
//! Source from opening.
//!
//! All files are opened with read access only, as everywhere in this crate (ADR-0003).

mod dmg;
mod ewf;
mod qcow2;
mod vhd;
mod vhdx;
mod vmdk;

use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::image::read_exact_at;
use crate::{BlockSource, Geometry, ImageSource, ReadError, SourceIdentity, SourceKind};

/// The on-disk format of an image file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContainerFormat {
    /// A plain sector-by-sector copy (`dd`, ddrescue, `rescyo image`).
    Raw,
    /// Microsoft Virtual PC / Hyper-V VHD: fixed, dynamic or differencing.
    Vhd,
    /// Hyper-V VHDX (with its metadata log replayed in memory).
    Vhdx,
    /// VMware VMDK: sparse, stream-optimized, or a descriptor with flat and sparse extents.
    Vmdk,
    /// QEMU qcow2, versions 2 and 3.
    Qcow2,
    /// Expert Witness / EnCase E01 (and SMART S01) evidence files, one or more segments.
    Ewf,
    /// Apple UDIF disk image (`.dmg`): raw, or ADC, zlib, bzip2, LZFSE or LZMA compressed.
    Dmg,
}

impl ContainerFormat {
    pub const ALL: [ContainerFormat; 7] = [
        ContainerFormat::Raw,
        ContainerFormat::Vhd,
        ContainerFormat::Vhdx,
        ContainerFormat::Vmdk,
        ContainerFormat::Qcow2,
        ContainerFormat::Ewf,
        ContainerFormat::Dmg,
    ];

    /// The short name used on the command line and in JSON (`raw`, `vhd`, `vhdx`, …).
    pub fn name(self) -> &'static str {
        match self {
            ContainerFormat::Raw => "raw",
            ContainerFormat::Vhd => "vhd",
            ContainerFormat::Vhdx => "vhdx",
            ContainerFormat::Vmdk => "vmdk",
            ContainerFormat::Qcow2 => "qcow2",
            ContainerFormat::Ewf => "e01",
            ContainerFormat::Dmg => "dmg",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let n = name.to_ascii_lowercase();
        match n.as_str() {
            "ewf" | "e01" | "s01" => Some(ContainerFormat::Ewf),
            "vpc" => Some(ContainerFormat::Vhd),
            "udif" => Some(ContainerFormat::Dmg),
            _ => Self::ALL.into_iter().find(|f| f.name() == n),
        }
    }
}

impl fmt::Display for ContainerFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why an image could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The container's own header is damaged and no copy of it is usable.
    #[error("{format}: {reason}")]
    Invalid { format: ContainerFormat, reason: String },
    /// A valid container that uses a feature Rescyo cannot read yet.
    #[error("{format}: {feature} is not supported")]
    Unsupported {
        format: ContainerFormat,
        feature: String,
    },
    /// A differencing image whose parent (backing) image cannot be found.
    #[error("{format}: parent image not found (tried {tried})")]
    ParentMissing { format: ContainerFormat, tried: String },
    /// A chain of parent images that is too deep or refers back to itself.
    #[error("{format}: parent chain is too deep or loops back to {path}")]
    ParentLoop { format: ContainerFormat, path: String },
}

impl OpenError {
    pub(crate) fn invalid(format: ContainerFormat, reason: impl Into<String>) -> Self {
        OpenError::Invalid {
            format,
            reason: reason.into(),
        }
    }

    pub(crate) fn unsupported(format: ContainerFormat, feature: impl Into<String>) -> Self {
        OpenError::Unsupported {
            format,
            feature: feature.into(),
        }
    }
}

/// What was learned while opening an image, for display and for Reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerInfo {
    pub format: ContainerFormat,
    /// Size of the virtual disk in bytes.
    pub virtual_size: u64,
    /// Every host file read, the image itself first, then its segments, extents and parents.
    pub files: Vec<PathBuf>,
    /// Damage repaired in memory or tolerated while opening (a bad header copy, an unapplied
    /// log, a checksum mismatch). Empty when the container is consistent.
    pub notes: Vec<String>,
}

/// Opens an image file of any supported format, detecting the format unless `format` is given.
///
/// `logical_sector` is used for raw images; containers state their own sector size (512 for
/// every format except 4Kn VHDX).
pub fn open_image(
    path: &Path,
    logical_sector: u32,
    format: Option<ContainerFormat>,
) -> Result<(Arc<dyn BlockSource>, ContainerInfo), OpenError> {
    let mut chain = Vec::new();
    open_in_chain(path, logical_sector, format, &mut chain)
}

/// Maximum number of parent images followed (VHD differencing, qcow2 backing, VMDK delta).
const MAX_PARENT_DEPTH: usize = 16;

fn open_in_chain(
    path: &Path,
    logical_sector: u32,
    format: Option<ContainerFormat>,
    chain: &mut Vec<PathBuf>,
) -> Result<(Arc<dyn BlockSource>, ContainerInfo), OpenError> {
    let format = match format {
        Some(f) => f,
        None => detect(path)?,
    };
    if format == ContainerFormat::Raw {
        let src = ImageSource::open(path, logical_sector)?;
        let info = ContainerInfo {
            format,
            virtual_size: src.geometry().size,
            files: vec![path.to_path_buf()],
            notes: Vec::new(),
        };
        return Ok((Arc::new(src), info));
    }
    let canonical = std::fs::canonicalize(path)?;
    if chain.contains(&canonical) || chain.len() >= MAX_PARENT_DEPTH {
        return Err(OpenError::ParentLoop {
            format,
            path: path.display().to_string(),
        });
    }
    chain.push(canonical);
    let opened = match format {
        ContainerFormat::Raw => return Err(OpenError::invalid(format, "raw is not a container")),
        ContainerFormat::Vhd => vhd::open(path)?,
        ContainerFormat::Vhdx => vhdx::open(path)?,
        ContainerFormat::Vmdk => vmdk::open(path)?,
        ContainerFormat::Qcow2 => qcow2::open(path)?,
        ContainerFormat::Ewf => ewf::open(path)?,
        ContainerFormat::Dmg => dmg::open(path)?,
    };
    let mut notes = opened.notes;
    let mut files: Vec<PathBuf> = opened.files.iter().map(|f| f.path.clone()).collect();
    let parent = match opened.parent {
        None => None,
        Some(want) => {
            let found = want
                .candidates
                .iter()
                .find(|c| c.is_file())
                .cloned()
                .ok_or_else(|| OpenError::ParentMissing {
                    format,
                    tried: want
                        .candidates
                        .iter()
                        .map(|c| c.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                })?;
            let (src, info) = open_in_chain(&found, 512, None, chain)?;
            if src.geometry().logical_sector != opened.logical_sector {
                return Err(OpenError::invalid(
                    format,
                    "parent image has a different sector size",
                ));
            }
            if let Some(check) = want.check
                && let Err(why) = check(&info)
            {
                notes.push(why);
            }
            files.extend(info.files);
            notes.extend(info.notes.into_iter().map(|n| format!("parent: {n}")));
            Some(src)
        }
    };
    let size = opened.layout.size();
    let ss = u64::from(opened.logical_sector);
    let geometry = Geometry {
        size: size - size % ss,
        logical_sector: opened.logical_sector,
    };
    let info = ContainerInfo {
        format,
        virtual_size: size,
        files,
        notes,
    };
    let src = ContainerSource {
        files: opened.files,
        layout: opened.layout,
        parent,
        geometry,
        identity: SourceIdentity {
            kind: SourceKind::Image,
            path: path.display().to_string(),
            model: None,
            serial: None,
        },
        cache: Mutex::new(ChunkCache::default()),
    };
    Ok((Arc::new(src), info))
}

/// Recognises a container by its signature; anything unrecognised is a raw image.
pub fn detect(path: &Path) -> io::Result<ContainerFormat> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut head = vec![0u8; usize::try_from(len.min(4096)).unwrap_or(4096)];
    read_exact_at(&file, 0, &mut head)?;
    let mut tail = vec![0u8; usize::try_from(len.min(512)).unwrap_or(512)];
    read_exact_at(&file, len - tail.len() as u64, &mut tail)?;
    Ok(detect_bytes(&head, &tail))
}

fn detect_bytes(head: &[u8], tail: &[u8]) -> ContainerFormat {
    let starts = |sig: &[u8]| head.starts_with(sig);
    if starts(b"vhdxfile") {
        ContainerFormat::Vhdx
    } else if starts(b"QFI\xfb") {
        ContainerFormat::Qcow2
    } else if starts(b"KDMV") || vmdk::looks_like_descriptor(head) {
        ContainerFormat::Vmdk
    } else if starts(ewf::SIGNATURE) || starts(ewf::SIGNATURE_SMART) {
        ContainerFormat::Ewf
    } else if tail.len() == 512 && tail.starts_with(dmg::TRAILER)
        || starts(dmg::ENCRYPTED)
        || starts(dmg::SPARSE)
    {
        ContainerFormat::Dmg
    } else if tail.len() == 512 && tail.starts_with(b"conectix") || starts(b"conectix") {
        ContainerFormat::Vhd
    } else {
        ContainerFormat::Raw
    }
}

/// A host file a container reads from.
#[derive(Debug)]
pub(crate) struct HostFile {
    pub path: PathBuf,
    pub file: File,
    pub len: u64,
}

impl HostFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
            len: meta.len(),
        })
    }

    /// Reads exactly `buf.len()` bytes at `offset`; a range past the end of the file is
    /// reported as [`ReadError::Media`] (a truncated container), not as an I/O failure.
    pub fn read(&self, offset: u64, buf: &mut [u8]) -> Result<(), ReadError> {
        let end = offset.checked_add(buf.len() as u64).ok_or(ReadError::Media)?;
        if end > self.len {
            return Err(ReadError::Media);
        }
        read_exact_at(&self.file, offset, buf).map_err(|e| match crate::image::classify(e) {
            ReadError::OutOfBounds => ReadError::Media,
            other => other,
        })
    }

    /// Reads a metadata structure while opening, as an [`OpenError`].
    pub fn read_vec(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        let end = offset
            .checked_add(len as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "offset overflow"))?;
        if end > self.len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{} is truncated", self.path.display()),
            ));
        }
        read_exact_at(&self.file, offset, &mut buf)?;
        Ok(buf)
    }
}

/// Where a run of virtual bytes comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Piece {
    /// Stored as-is in host file `file` at `offset`.
    Data { file: usize, offset: u64 },
    /// Never written: reads as zeros.
    Zero,
    /// Not stored here: read the parent image at the same virtual offset (zeros without one).
    Parent,
    /// Stored compressed: byte `within` of decoded chunk `chunk`.
    Chunk { chunk: u64, within: u64 },
    /// The table describing this range is damaged: unreadable.
    Bad,
}

/// A container format's map from virtual offsets to host bytes.
pub(crate) trait Layout: Send + Sync + fmt::Debug {
    /// Size of the virtual disk in bytes.
    fn size(&self) -> u64;

    /// Where virtual byte `offset` lives, and for how many bytes from `offset` (at least one
    /// sector, never past [`Layout::size`]) that answer holds.
    fn locate(&self, files: &[HostFile], offset: u64) -> Result<(Piece, u64), ReadError>;

    /// Decodes compressed chunk `chunk`. Called only for chunks [`Layout::locate`] returned.
    fn decode(&self, _files: &[HostFile], _chunk: u64) -> Result<Vec<u8>, ReadError> {
        Err(ReadError::Media)
    }
}

/// A parent image a container needs, as candidate paths in order of preference.
pub(crate) struct ParentRef {
    pub candidates: Vec<PathBuf>,
    /// Checks the opened parent (e.g. its unique id); an error becomes a note.
    pub check: Option<ParentCheck>,
}

/// A check of an opened parent image; its error message becomes a note.
pub(crate) type ParentCheck = Box<dyn Fn(&ContainerInfo) -> Result<(), String>>;

/// What a format's `open` returns.
pub(crate) struct Opened {
    pub files: Vec<HostFile>,
    pub layout: Box<dyn Layout>,
    pub logical_sector: u32,
    pub parent: Option<ParentRef>,
    pub notes: Vec<String>,
}

/// The last few decoded chunks: reads arrive sector by sector, chunks are 32–64 KiB.
#[derive(Debug, Default)]
struct ChunkCache {
    entries: Vec<(u64, Arc<Vec<u8>>)>,
}

impl ChunkCache {
    const CAPACITY: usize = 8;

    fn get(&mut self, chunk: u64) -> Option<Arc<Vec<u8>>> {
        let pos = self.entries.iter().position(|(c, _)| *c == chunk)?;
        let hit = self.entries.remove(pos);
        let data = Arc::clone(&hit.1);
        self.entries.push(hit);
        Some(data)
    }

    fn put(&mut self, chunk: u64, data: Arc<Vec<u8>>) {
        if self.entries.len() >= Self::CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push((chunk, data));
    }
}

/// A container image as a [`BlockSource`] of its virtual disk.
pub struct ContainerSource {
    files: Vec<HostFile>,
    layout: Box<dyn Layout>,
    parent: Option<Arc<dyn BlockSource>>,
    geometry: Geometry,
    identity: SourceIdentity,
    cache: Mutex<ChunkCache>,
}

impl fmt::Debug for ContainerSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContainerSource")
            .field("identity", &self.identity)
            .field("geometry", &self.geometry)
            .field("layout", &self.layout)
            .field("parent", &self.parent.as_ref().map(|p| p.identity().path.clone()))
            .finish_non_exhaustive()
    }
}

impl ContainerSource {
    fn chunk(&self, chunk: u64) -> Result<Arc<Vec<u8>>, ReadError> {
        if let Ok(mut c) = self.cache.lock()
            && let Some(hit) = c.get(chunk)
        {
            return Ok(hit);
        }
        let data = Arc::new(self.layout.decode(&self.files, chunk)?);
        if let Ok(mut c) = self.cache.lock() {
            c.put(chunk, Arc::clone(&data));
        }
        Ok(data)
    }
}

impl BlockSource for ContainerSource {
    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn identity(&self) -> &SourceIdentity {
        &self.identity
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), ReadError> {
        let ss = u64::from(self.geometry.logical_sector);
        if !crate::is_aligned(offset, buf.len(), ss) {
            return Err(ReadError::Misaligned);
        }
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(ReadError::OutOfBounds)?;
        if end > self.geometry.size {
            return Err(ReadError::OutOfBounds);
        }
        let mut pos = 0usize;
        while pos < buf.len() {
            let at = offset + pos as u64;
            let (piece, run) = self.layout.locate(&self.files, at)?;
            let left = buf.len() - pos;
            let n = usize::try_from(run).map_or(left, |r| r.min(left));
            if n == 0 {
                return Err(ReadError::Media);
            }
            let out = buf.get_mut(pos..pos + n).ok_or(ReadError::OutOfBounds)?;
            match piece {
                Piece::Data { file, offset } => {
                    self.files.get(file).ok_or(ReadError::Media)?.read(offset, out)?;
                }
                Piece::Zero => out.fill(0),
                Piece::Parent => match &self.parent {
                    // A parent may be smaller than its child (qcow2 allows growing a disk on
                    // top of a backing file): past its end the child reads zeros.
                    Some(p) => {
                        let have = p.geometry().size.saturating_sub(at);
                        let k = usize::try_from(have).map_or(n, |h| h.min(n));
                        let (inside, beyond) = out.split_at_mut(k);
                        if !inside.is_empty() {
                            p.read_at(at, inside)?;
                        }
                        beyond.fill(0);
                    }
                    None => out.fill(0),
                },
                Piece::Chunk { chunk, within } => {
                    let data = self.chunk(chunk)?;
                    let from = usize::try_from(within).map_err(|_| ReadError::Media)?;
                    let src = data.get(from..from + n).ok_or(ReadError::Media)?;
                    out.copy_from_slice(src);
                }
                Piece::Bad => return Err(ReadError::Media),
            }
            pos += n;
        }
        Ok(())
    }
}

/// Decompresses a raw DEFLATE stream (qcow2) into at most `limit` bytes.
pub(crate) fn inflate_raw(data: &[u8], limit: usize) -> Result<Vec<u8>, ReadError> {
    miniz_oxide::inflate::decompress_to_vec_with_limit(data, limit).map_err(|_| ReadError::Media)
}

/// Decompresses a zlib stream (VMDK grains, EWF chunks) into at most `limit` bytes; the
/// stream's own Adler-32 is verified.
pub(crate) fn inflate_zlib(data: &[u8], limit: usize) -> Result<Vec<u8>, ReadError> {
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, limit).map_err(|_| ReadError::Media)
}

/// The directory an image lives in, for resolving relative parent and extent paths.
pub(crate) fn dir_of(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Turns a path written by another OS (`C:\vm\base.vhd`, `./base.vhd`, `file://localhost/…`)
/// into candidates on this machine: as written, then its file name next to the child image.
pub(crate) fn foreign_path_candidates(dir: &Path, written: &str) -> Vec<PathBuf> {
    let mut s = written.trim().trim_matches('\0').to_string();
    if let Some(rest) = s.strip_prefix("file://") {
        s = rest.strip_prefix("localhost").unwrap_or(rest).to_string();
    }
    let unified = s.replace('\\', "/");
    let mut out = Vec::new();
    let bytes = unified.as_bytes();
    let drive_letter =
        bytes.len() > 2 && bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':');
    let as_written = Path::new(&unified);
    if as_written.is_absolute() {
        out.push(as_written.to_path_buf());
    } else if drive_letter {
        // A Windows absolute path read on another OS: only its file name can help.
    } else if !unified.is_empty() {
        out.push(dir.join(unified.trim_start_matches("./")));
    }
    if let Some(name) = unified.rsplit('/').next().filter(|n| !n.is_empty()) {
        let beside = dir.join(name);
        if !out.contains(&beside) {
            out.push(beside);
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::io::Write;

    /// Writes `bytes` to a new temporary file inside `dir` named `name`.
    pub fn file_in(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut f = tempfile::NamedTempFile::new_in(dir).unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f.persist(&p).unwrap();
        p
    }

    /// Reads the whole virtual disk of `src`.
    pub fn read_all(src: &dyn crate::BlockSource) -> Vec<u8> {
        let mut out = vec![0u8; usize::try_from(src.geometry().size).unwrap()];
        src.read_at(0, &mut out).unwrap();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for f in ContainerFormat::ALL {
            assert_eq!(ContainerFormat::from_name(f.name()), Some(f));
        }
        assert_eq!(ContainerFormat::from_name("VPC"), Some(ContainerFormat::Vhd));
        assert_eq!(ContainerFormat::from_name("E01"), Some(ContainerFormat::Ewf));
        assert_eq!(ContainerFormat::from_name("iso"), None);
    }

    #[test]
    fn unknown_bytes_are_raw() {
        assert_eq!(detect_bytes(&[0u8; 4096], &[0u8; 512]), ContainerFormat::Raw);
        assert_eq!(detect_bytes(b"QFI\xfb\0\0\0\x03", &[]), ContainerFormat::Qcow2);
        assert_eq!(detect_bytes(b"vhdxfile", &[]), ContainerFormat::Vhdx);
        assert_eq!(detect_bytes(b"KDMV\x01\0\0\0", &[]), ContainerFormat::Vmdk);
        let mut koly = [0u8; 512];
        koly[..4].copy_from_slice(b"koly");
        assert_eq!(detect_bytes(&[0u8; 4096], &koly), ContainerFormat::Dmg);
    }

    #[test]
    fn foreign_paths_become_local_candidates() {
        let dir = Path::new("/imgs");
        #[cfg(unix)]
        assert_eq!(
            foreign_path_candidates(dir, r"C:\vm\base.vhd"),
            vec![dir.join("base.vhd")]
        );
        assert_eq!(
            foreign_path_candidates(dir, ".\\base.vhd"),
            vec![dir.join("base.vhd")]
        );
        assert_eq!(
            foreign_path_candidates(dir, "file://localhost/Users/me/base.vhd"),
            vec![PathBuf::from("/Users/me/base.vhd"), dir.join("base.vhd")]
        );
    }

    #[test]
    fn opens_raw_images_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let p = testutil::file_in(dir.path(), "disk.img", &crate::testing::pattern(8192));
        let (src, info) = open_image(&p, 512, None).unwrap();
        assert_eq!(info.format, ContainerFormat::Raw);
        assert_eq!(testutil::read_all(src.as_ref()), crate::testing::pattern(8192));
    }
}
