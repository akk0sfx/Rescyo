//! Types shared by every engine and by the Session store.

use serde::{Deserialize, Serialize};

use crate::Timestamp;

/// File-system families Rescyo understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FsKind {
    Fat12,
    Fat16,
    Fat32,
    Exfat,
    Ntfs,
}

impl FsKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fat12 => "fat12",
            Self::Fat16 => "fat16",
            Self::Fat32 => "fat32",
            Self::Exfat => "exfat",
            Self::Ntfs => "ntfs",
        }
    }

    pub fn is_fat(self) -> bool {
        matches!(self, Self::Fat12 | Self::Fat16 | Self::Fat32)
    }
}

/// One piece of evidence for a Volume candidate's geometry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    /// A valid boot sector at this byte offset.
    BootSector { offset: u64 },
    /// A valid backup boot sector at this byte offset.
    BackupBootSector { offset: u64 },
    /// A partition table entry with this number points at the volume.
    PartitionEntry { number: u32 },
    /// A copy of the allocation table found by its signature.
    FatSignature { offset: u64 },
    /// Directory starts whose positions agree with this geometry.
    DirectoryVotes { count: u64 },
    /// MFT records whose positions agree with this geometry.
    MftRecords { count: u64 },
    /// An external Hint (e.g. a DMDE scan log) proposed this geometry.
    Hint { source: String },
}

/// A hypothesis that a file system with a given geometry exists on the Source.
///
/// `params` carries what the engine needs to open it; it is opaque to everyone else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeCandidate {
    pub fs: FsKind,
    /// Byte offset of the volume start (its boot sector), when known.
    pub offset: Option<u64>,
    /// Bytes per cluster.
    pub cluster_size: u32,
    /// Byte offset of the first data cluster (FAT/exFAT cluster #2, NTFS cluster 0).
    pub data_offset: u64,
    /// Volume size in bytes, when known.
    pub size: Option<u64>,
    pub label: Option<String>,
    pub evidence: Vec<Evidence>,
    /// Higher is more trustworthy; comparable within one Scan.
    pub score: u64,
    pub params: serde_json::Value,
}

impl VolumeCandidate {
    /// Two candidates describe the same volume when family and cluster grid agree.
    pub fn same_volume(&self, other: &Self) -> bool {
        self.fs.is_fat() == other.fs.is_fat()
            && (self.fs == other.fs || (self.fs.is_fat() && other.fs.is_fat()))
            && self.cluster_size == other.cluster_size
            && self.data_offset == other.data_offset
    }
}

/// A directory start to begin tree reconstruction from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectorySeed {
    /// FAT/exFAT cluster number of the directory's first cluster.
    pub cluster: u64,
    /// The parent cluster it claims (0 = root), if known.
    pub parent: Option<u64>,
    pub origin: SeedOrigin,
}

/// Where a directory start came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedOrigin {
    /// Reached by walking from the root.
    Walk,
    /// Found by Rescyo's own scan.
    OwnScan,
    /// Proposed by a DMDE scan log and confirmed on the Source.
    DmdeHint,
}

/// Opaque id assigned by a [`crate::ScanSink`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntryId(pub u64);

/// Where an entry hangs in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Parent {
    /// Directly in the volume root.
    Root,
    /// Inside another entry.
    Entry(EntryId),
    /// A Lost folder: its parent could not be reached. `parent_ref` is the parent's cluster or
    /// record number as claimed on disk (0 if unknown).
    Lost { parent_ref: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
    /// A named data stream of a file (NTFS alternate data stream), named `file:stream` and
    /// placed beside its file.
    Stream,
}

/// Whether the file system still considers the entry allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    Live,
    Deleted,
}

/// How an Entry's byte extents were determined, most to least trustworthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssemblyMethod {
    FatChain,
    FatChainMirror,
    ExfatBitmapContiguous,
    MftRunlist,
    ContiguousAssumed,
    Carved,
}

impl AssemblyMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FatChain => "fat_chain",
            Self::FatChainMirror => "fat_chain_mirror",
            Self::ExfatBitmapContiguous => "exfat_bitmap_contiguous",
            Self::MftRunlist => "mft_runlist",
            Self::ContiguousAssumed => "contiguous_assumed",
            Self::Carved => "carved",
        }
    }

    pub fn is_guess(self) -> bool {
        matches!(self, Self::ContiguousAssumed | Self::Carved)
    }
}

/// One piece of a file's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Extent {
    /// `len` bytes at absolute Source offset `offset`.
    Disk { offset: u64, len: u64 },
    /// `len` zero bytes (sparse or deliberately unallocated).
    Zero { len: u64 },
    /// Bytes stored inside the metadata itself (NTFS resident data).
    Inline { data: Vec<u8> },
    /// `len` bytes that are `stored` in compressed form and must be decoded with `codec`.
    Encoded {
        codec: Codec,
        stored: Vec<Extent>,
        len: u64,
    },
}

/// How an [`Extent::Encoded`] is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    /// One NTFS compression unit in LZNT1.
    Lznt1,
    /// A whole WOF-compressed stream (chunk table and chunks) in XPRESS Huffman, 4 KiB chunks.
    WofXpress4k,
    WofXpress8k,
    WofXpress16k,
    /// A whole WOF-compressed stream in LZX, 32 KiB chunks.
    WofLzx,
}

impl Extent {
    pub fn len(&self) -> u64 {
        match self {
            Self::Disk { len, .. } | Self::Zero { len } | Self::Encoded { len, .. } => *len,
            Self::Inline { data } => data.len() as u64,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Exactly which bytes make up a file, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataLayout {
    pub extents: Vec<Extent>,
}

impl DataLayout {
    pub fn len(&self) -> u64 {
        self.extents.iter().map(Extent::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Appends a disk run, merging it with the previous run when adjacent.
    pub fn push_disk(&mut self, offset: u64, len: u64) {
        if len == 0 {
            return;
        }
        if let Some(Extent::Disk { offset: o, len: l }) = self.extents.last_mut()
            && *o + *l == offset
        {
            *l += len;
            return;
        }
        self.extents.push(Extent::Disk { offset, len });
    }
}

/// File timestamps as stored; `None` when absent or invalid on disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Times {
    pub created: Option<Timestamp>,
    pub modified: Option<Timestamp>,
    pub accessed: Option<Timestamp>,
}

/// A file or directory found in a Session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub parent: Parent,
    pub name: String,
    /// Undecoded short-name bytes when the name came from an OEM code page.
    pub name_raw: Option<Vec<u8>>,
    pub kind: EntryKind,
    pub state: EntryState,
    pub size: u64,
    pub times: Times,
    /// Empty for directories.
    pub layout: DataLayout,
    pub assembly: AssemblyMethod,
    /// First cluster (FAT/exFAT) or MFT record number: the engine's own reference.
    pub fs_ref: u64,
    /// For directories: how their start was found.
    pub seed: Option<SeedOrigin>,
}

/// Phases reported through [`crate::ScanSink::progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Probe,
    DeepScan,
    Walk,
    LostFolders,
}

/// Something an engine wants recorded in the Session journal and Report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Note {
    /// The primary boot sector was unusable and a backup was used.
    BackupBootSectorUsed { offset: u64 },
    /// No boot sector: the geometry was inferred.
    GeometryInferred,
    /// The primary allocation table is damaged; the mirror is used where it agrees.
    FatMirrorUsed,
    /// No usable allocation table at all.
    NoFatTable,
    /// A directory continued into the next cluster without allocation-table evidence.
    DirectoryContinuedHeuristically { cluster: u64 },
    /// A directory continued into a non-adjacent cluster because an entry set split across the
    /// boundary verified by its checksum.
    DirectoryStitched { cluster: u64 },
    /// A cycle or out-of-range link was cut.
    ChainCut { cluster: u64 },
    /// A limit from [`crate::Limits`] stopped work.
    LimitReached { what: String },
    /// Short names were decoded with this code page.
    Codepage { name: String, detected: bool },
    /// Directory seeds that did not hold a directory and were dropped.
    SeedsRejected { count: u64 },
    /// Lost folders attached.
    LostFolders { count: u64 },
}
