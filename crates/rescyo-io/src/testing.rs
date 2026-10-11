//! Test doubles: an in-memory Source and a fault-injecting wrapper.
//!
//! Enabled for this crate's tests and, through the `testing` feature, for other crates'
//! dev-dependencies. Never compiled into release binaries.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::{BlockSource, Geometry, ReadError, SourceIdentity, SourceKind};

/// A Source backed by a byte vector.
#[derive(Debug)]
pub struct MemSource {
    data: Vec<u8>,
    geometry: Geometry,
    identity: SourceIdentity,
}

impl MemSource {
    pub fn new(data: Vec<u8>, logical_sector: u32) -> Self {
        let geometry = Geometry {
            size: data.len() as u64,
            logical_sector,
        };
        let identity = SourceIdentity {
            kind: SourceKind::Test,
            path: "mem".into(),
            model: None,
            serial: None,
        };
        Self {
            data,
            geometry,
            identity,
        }
    }

    pub fn with_identity(mut self, identity: SourceIdentity) -> Self {
        self.identity = identity;
        self
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl BlockSource for MemSource {
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
        let start = usize::try_from(offset).map_err(|_| ReadError::OutOfBounds)?;
        let end = start.checked_add(buf.len()).ok_or(ReadError::OutOfBounds)?;
        let src = self.data.get(start..end).ok_or(ReadError::OutOfBounds)?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

/// Wraps a Source and injects failures by sector, counting every physical read attempt.
#[derive(Debug)]
pub struct FaultySource<S> {
    inner: S,
    bad: Vec<Range<u64>>,
    slow: Vec<(Range<u64>, Duration)>,
    gone_after: Option<u64>,
    reads: AtomicU64,
    sector_reads: Mutex<HashMap<u64, u64>>,
}

impl<S: BlockSource> FaultySource<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            bad: Vec::new(),
            slow: Vec::new(),
            gone_after: None,
            reads: AtomicU64::new(0),
            sector_reads: Mutex::new(HashMap::new()),
        }
    }

    /// Sectors in `sectors` fail with [`ReadError::Media`].
    pub fn bad(mut self, sectors: Range<u64>) -> Self {
        self.bad.push(sectors);
        self
    }

    /// Reads touching `sectors` take `delay`.
    pub fn slow(mut self, sectors: Range<u64>, delay: Duration) -> Self {
        self.slow.push((sectors, delay));
        self
    }

    /// After `n` successful-or-not read calls, every read fails with [`ReadError::Gone`].
    pub fn gone_after(mut self, n: u64) -> Self {
        self.gone_after = Some(n);
        self
    }

    /// Number of `read_at` calls so far.
    pub fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }

    /// How many read calls covered `sector`.
    pub fn reads_of(&self, sector: u64) -> u64 {
        self.sector_reads
            .lock()
            .map(|m| m.get(&sector).copied().unwrap_or(0))
            .unwrap_or(0)
    }
}

fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.start < b.end && b.start < a.end
}

impl<S: BlockSource> BlockSource for FaultySource<S> {
    fn geometry(&self) -> Geometry {
        self.inner.geometry()
    }

    fn identity(&self) -> &SourceIdentity {
        self.inner.identity()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), ReadError> {
        let n = self.reads.fetch_add(1, Ordering::SeqCst);
        if self.gone_after.is_some_and(|limit| n >= limit) {
            return Err(ReadError::Gone(std::io::Error::other("device removed")));
        }
        let ss = u64::from(self.inner.geometry().logical_sector);
        let sectors = offset / ss..(offset + buf.len() as u64).div_ceil(ss);
        if let Ok(mut m) = self.sector_reads.lock() {
            for s in sectors.clone() {
                *m.entry(s).or_insert(0) += 1;
            }
        }
        for (range, delay) in &self.slow {
            if overlaps(range, &sectors) {
                std::thread::sleep(*delay);
            }
        }
        if self.bad.iter().any(|b| overlaps(b, &sectors)) {
            return Err(ReadError::Media);
        }
        self.inner.read_at(offset, buf)
    }
}

/// Bytes `0, 1, 2, …` wrapping at 251 (a prime, so sector contents differ), for `len` bytes.
pub fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| u8::try_from(i % 251).unwrap_or(0)).collect()
}
