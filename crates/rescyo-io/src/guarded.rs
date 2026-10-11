//! The only way engines read a Source: alignment, unreadable sectors, the Bad-sector map and the
//! degradation alarm in one place.

use std::io;
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::{BadSectorMap, BlockSource, Geometry, ReadError, SourceIdentity};

/// How gently to treat the medium.
#[derive(Debug, Clone)]
pub struct ReadPolicy {
    /// Largest single request sent to the Source, in bytes (rounded down to whole sectors).
    pub max_block: usize,
    /// Extra attempts for a sector that failed once. 0 means read each sector exactly once.
    pub retries: u32,
    /// Re-read sectors already in the Bad-sector map. Off by default: never hammer known damage.
    pub reread_known_bad: bool,
    /// Raise the degradation alarm after this many newly unreadable sectors; `None` never does.
    pub alarm_after_errors: Option<u64>,
}

impl Default for ReadPolicy {
    fn default() -> Self {
        Self {
            max_block: 1 << 20,
            retries: 0,
            reread_known_bad: false,
            alarm_after_errors: Some(16),
        }
    }
}

/// What a successful read could not deliver.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadReport {
    /// Byte ranges of the request (absolute Source offsets) that were zero-filled.
    pub unreadable: Vec<Range<u64>>,
}

impl ReadReport {
    pub fn is_clean(&self) -> bool {
        self.unreadable.is_empty()
    }

    pub fn unreadable_bytes(&self) -> u64 {
        self.unreadable.iter().map(crate::range_len).sum()
    }

    fn push(&mut self, r: Range<u64>) {
        if r.start >= r.end {
            return;
        }
        if let Some(last) = self.unreadable.last_mut()
            && last.end == r.start
        {
            last.end = r.end;
            return;
        }
        self.unreadable.push(r);
    }
}

/// Why a guarded read stopped.
#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    /// The request reaches past the end of the Source: the caller followed a bogus pointer.
    #[error("read beyond the end of the source")]
    OutOfBounds,
    /// Too many unreadable sectors: stop and advise imaging. Sticky until acknowledged.
    #[error("degradation alarm: {errors} unreadable sectors since the last acknowledgement")]
    DegradationAlarm { errors: u64 },
    /// The device disappeared.
    #[error("source is gone: {0}")]
    Gone(io::Error),
    #[error(transparent)]
    Io(io::Error),
}

#[derive(Debug, Default)]
struct State {
    map: BadSectorMap,
    errors_since_ack: u64,
    total_errors: u64,
}

/// Wraps a [`BlockSource`] with the gentle read policy. Cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct GuardedReader {
    source: Arc<dyn BlockSource>,
    geometry: Geometry,
    policy: ReadPolicy,
    state: Mutex<State>,
    in_flight: Mutex<Option<Instant>>,
}

impl GuardedReader {
    pub fn new(source: Arc<dyn BlockSource>, policy: ReadPolicy) -> Self {
        Self::with_map(source, policy, BadSectorMap::new())
    }

    /// Starts from a Bad-sector map saved by an earlier Scan.
    pub fn with_map(source: Arc<dyn BlockSource>, policy: ReadPolicy, map: BadSectorMap) -> Self {
        let geometry = source.geometry();
        let state = State {
            map,
            ..State::default()
        };
        Self {
            source,
            geometry,
            policy,
            state: Mutex::new(state),
            in_flight: Mutex::new(None),
        }
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    pub fn identity(&self) -> &SourceIdentity {
        self.source.identity()
    }

    pub fn source(&self) -> &Arc<dyn BlockSource> {
        &self.source
    }

    /// Snapshot of the Bad-sector map.
    pub fn bad_map(&self) -> BadSectorMap {
        self.state().map.clone()
    }

    /// Unreadable sectors found through this reader (not counting a pre-loaded map).
    pub fn error_count(&self) -> u64 {
        self.state().total_errors
    }

    /// Lets reading continue after a degradation alarm; the next alarm needs as many new errors.
    pub fn acknowledge_alarm(&self) {
        self.state().errors_since_ack = 0;
    }

    /// How long the current request to the device has been outstanding, if one is.
    pub fn stalled_for(&self) -> Option<Duration> {
        self.in_flight.lock().ok().and_then(|g| g.map(|t| t.elapsed()))
    }

    /// Reads `buf.len()` bytes at any `offset`. Unreadable sectors come back as zeros and are
    /// listed in the report.
    pub fn read(&self, offset: u64, buf: &mut [u8]) -> Result<ReadReport, GuardError> {
        let mut report = ReadReport::default();
        if buf.is_empty() {
            return Ok(report);
        }
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(GuardError::OutOfBounds)?;
        if end > self.geometry.size {
            return Err(GuardError::OutOfBounds);
        }
        self.check_alarm()?;

        let ss = u64::from(self.geometry.logical_sector);
        let win_start = offset - offset % ss;
        let win_end = end.div_ceil(ss) * ss;
        if win_start == offset && win_end == end {
            self.fill(win_start, buf, offset..end, &mut report)?;
        } else {
            let len = usize::try_from(win_end - win_start).map_err(|_| GuardError::OutOfBounds)?;
            let mut window = vec![0u8; len];
            self.fill(win_start, &mut window, offset..end, &mut report)?;
            let skip = usize::try_from(offset - win_start).map_err(|_| GuardError::OutOfBounds)?;
            let part = window
                .get(skip..skip + buf.len())
                .ok_or(GuardError::OutOfBounds)?;
            buf.copy_from_slice(part);
        }
        Ok(report)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A poisoned lock only means another reader panicked; the map itself stays consistent.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn check_alarm(&self) -> Result<(), GuardError> {
        if let Some(limit) = self.policy.alarm_after_errors {
            let errors = self.state().errors_since_ack;
            if errors >= limit {
                return Err(GuardError::DegradationAlarm { errors });
            }
        }
        Ok(())
    }

    /// Fills the sector-aligned `buf` that starts at `start`; `wanted` is the caller's byte range,
    /// used to clip the report.
    fn fill(
        &self,
        start: u64,
        buf: &mut [u8],
        wanted: Range<u64>,
        report: &mut ReadReport,
    ) -> Result<(), GuardError> {
        let ss = u64::from(self.geometry.logical_sector);
        let block = (self.policy.max_block as u64 / ss).max(1) * ss;
        let first_sector = start / ss;
        let sectors = buf.len() as u64 / ss;

        // Split the window into runs that are known-bad (skipped) or to be read.
        let known_bad: Vec<Range<u64>> = if self.policy.reread_known_bad {
            Vec::new()
        } else {
            self.state()
                .map
                .overlapping(first_sector..first_sector + sectors)
                .collect()
        };
        let mut cursor = first_sector;
        let mut runs: Vec<(Range<u64>, bool)> = Vec::new();
        for bad in known_bad {
            if cursor < bad.start {
                runs.push((cursor..bad.start, false));
            }
            runs.push((bad.clone(), true));
            cursor = bad.end;
        }
        if cursor < first_sector + sectors {
            runs.push((cursor..first_sector + sectors, false));
        }

        for (run, is_known_bad) in runs {
            let lo = slice_index(run.start - first_sector, ss)?;
            let hi = slice_index(run.end - first_sector, ss)?;
            let part = buf.get_mut(lo..hi).ok_or(GuardError::OutOfBounds)?;
            if is_known_bad {
                part.fill(0);
                report.push(clip(run.start * ss..run.end * ss, &wanted));
                continue;
            }
            let mut pos = 0usize;
            let run_start_byte = run.start * ss;
            while pos < part.len() {
                let n = (part.len() - pos).min(usize::try_from(block).unwrap_or(usize::MAX));
                let chunk = part.get_mut(pos..pos + n).ok_or(GuardError::OutOfBounds)?;
                self.read_block(run_start_byte + pos as u64, chunk, &wanted, report)?;
                pos += n;
            }
        }
        Ok(())
    }

    fn read_block(
        &self,
        offset: u64,
        buf: &mut [u8],
        wanted: &Range<u64>,
        report: &mut ReadReport,
    ) -> Result<(), GuardError> {
        match self.timed_read(offset, buf) {
            Ok(()) => return Ok(()),
            Err(ReadError::Media) => {}
            Err(e) => return Err(lift(e)),
        }
        // The block failed: go sector by sector, each read at most 1 + retries times.
        let ss = self.geometry.logical_sector as usize;
        for (i, sector) in buf.chunks_mut(ss).enumerate() {
            let at = offset + (i * ss) as u64;
            let mut ok = false;
            for _ in 0..=self.policy.retries {
                match self.timed_read(at, sector) {
                    Ok(()) => {
                        ok = true;
                        break;
                    }
                    Err(ReadError::Media) => {}
                    Err(e) => return Err(lift(e)),
                }
            }
            if !ok {
                sector.fill(0);
                let s = at / ss as u64;
                {
                    let mut st = self.state();
                    st.map.mark(s..s + 1);
                    st.errors_since_ack += 1;
                    st.total_errors += 1;
                }
                report.push(clip(at..at + ss as u64, wanted));
                self.check_alarm()?;
            }
        }
        Ok(())
    }

    fn timed_read(&self, offset: u64, buf: &mut [u8]) -> Result<(), ReadError> {
        if let Ok(mut g) = self.in_flight.lock() {
            *g = Some(Instant::now());
        }
        let r = self.source.read_at(offset, buf);
        if let Ok(mut g) = self.in_flight.lock() {
            *g = None;
        }
        r
    }
}

fn slice_index(sectors: u64, ss: u64) -> Result<usize, GuardError> {
    usize::try_from(sectors * ss).map_err(|_| GuardError::OutOfBounds)
}

fn clip(r: Range<u64>, to: &Range<u64>) -> Range<u64> {
    r.start.max(to.start)..r.end.min(to.end)
}

fn lift(e: ReadError) -> GuardError {
    match e {
        ReadError::OutOfBounds => GuardError::OutOfBounds,
        ReadError::Gone(e) => GuardError::Gone(e),
        ReadError::Io(e) => GuardError::Io(e),
        ReadError::Misaligned => GuardError::Io(io::Error::other("internal: misaligned read")),
        ReadError::Media => GuardError::Io(io::Error::other("internal: unhandled media error")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FaultySource, MemSource, pattern};

    const SS: u64 = 512;

    fn faulty(len: usize) -> FaultySource<MemSource> {
        FaultySource::new(MemSource::new(pattern(len), 512))
    }

    fn reader<S: BlockSource + 'static>(s: S, policy: ReadPolicy) -> (Arc<S>, GuardedReader) {
        let s = Arc::new(s);
        let r = GuardedReader::new(s.clone(), policy);
        (s, r)
    }

    #[test]
    fn unaligned_clean_read_returns_exact_bytes() {
        let (_, r) = reader(faulty(64 * 1024), ReadPolicy::default());
        let mut buf = vec![0; 1000];
        let rep = r.read(777, &mut buf).unwrap();
        assert!(rep.is_clean());
        assert_eq!(buf, pattern(64 * 1024)[777..1777]);
    }

    #[test]
    fn large_reads_are_chunked_by_max_block() {
        let policy = ReadPolicy {
            max_block: 1024,
            ..ReadPolicy::default()
        };
        let (s, r) = reader(faulty(16 * 1024), policy);
        let mut buf = vec![0; 8192];
        r.read(0, &mut buf).unwrap();
        assert_eq!(buf, pattern(16 * 1024)[..8192]);
        assert_eq!(s.reads(), 8);
    }

    #[test]
    fn bad_sector_is_zero_filled_reported_and_mapped() {
        let (s, r) = reader(faulty(16 * 1024).bad(5..6), ReadPolicy::default());
        let mut buf = vec![0xAA; 4096];
        let rep = r.read(0, &mut buf).unwrap();
        assert_eq!(rep.unreadable, vec![5 * SS..6 * SS]);
        assert!(buf[5 * 512..6 * 512].iter().all(|&b| b == 0));
        let want = pattern(16 * 1024);
        assert_eq!(buf[..5 * 512], want[..5 * 512]);
        assert_eq!(buf[6 * 512..], want[6 * 512..4096]);
        assert!(r.bad_map().is_bad(5));
        // One attempt in the block read and exactly one in the sector pass.
        assert_eq!(s.reads_of(5), 2);
    }

    #[test]
    fn known_bad_sectors_are_never_reread() {
        let (s, r) = reader(faulty(16 * 1024).bad(5..6), ReadPolicy::default());
        let mut buf = vec![0; 4096];
        r.read(0, &mut buf).unwrap();
        let before = s.reads_of(5);
        let rep = r.read(0, &mut buf).unwrap();
        assert_eq!(rep.unreadable, vec![5 * SS..6 * SS]);
        assert_eq!(s.reads_of(5), before);
    }

    #[test]
    fn preloaded_map_is_honoured() {
        let s = Arc::new(faulty(16 * 1024));
        let map: BadSectorMap = std::iter::once(3..4).collect();
        let r = GuardedReader::with_map(s.clone(), ReadPolicy::default(), map);
        let mut buf = vec![0xFF; 4096];
        let rep = r.read(0, &mut buf).unwrap();
        assert_eq!(rep.unreadable, vec![3 * SS..4 * SS]);
        assert_eq!(s.reads_of(3), 0);
        assert_eq!(r.error_count(), 0);
    }

    #[test]
    fn retries_are_opt_in() {
        let policy = ReadPolicy {
            retries: 2,
            ..ReadPolicy::default()
        };
        let (s, r) = reader(faulty(4096).bad(1..2), policy);
        let mut buf = vec![0; 4096];
        r.read(0, &mut buf).unwrap();
        assert_eq!(s.reads_of(1), 1 + 3);
    }

    #[test]
    fn report_is_clipped_to_the_request() {
        let (_, r) = reader(faulty(4096).bad(1..2), ReadPolicy::default());
        let mut buf = vec![0; 100];
        let rep = r.read(600, &mut buf).unwrap();
        assert_eq!(rep.unreadable, vec![600..700]);
    }

    #[test]
    fn degradation_alarm_trips_and_is_sticky_until_acknowledged() {
        let mut src = faulty(64 * 1024);
        for s in 0..20 {
            src = src.bad(s * 4..s * 4 + 1);
        }
        let policy = ReadPolicy {
            alarm_after_errors: Some(16),
            ..ReadPolicy::default()
        };
        let (_, r) = reader(src, policy);
        let mut buf = vec![0; 64 * 1024];
        assert!(matches!(
            r.read(0, &mut buf),
            Err(GuardError::DegradationAlarm { errors: 16 })
        ));
        let mut small = vec![0; 512];
        assert!(matches!(
            r.read(60_000, &mut small),
            Err(GuardError::DegradationAlarm { .. })
        ));
        r.acknowledge_alarm();
        let rep = r.read(0, &mut buf).unwrap();
        assert_eq!(rep.unreadable.len(), 20);
        assert_eq!(r.error_count(), 20);
    }

    #[test]
    fn continue_on_errors_never_alarms() {
        let mut src = faulty(64 * 1024);
        for s in 0..40 {
            src = src.bad(s * 2..s * 2 + 1);
        }
        let policy = ReadPolicy {
            alarm_after_errors: None,
            ..ReadPolicy::default()
        };
        let (_, r) = reader(src, policy);
        let mut buf = vec![0; 64 * 1024];
        assert_eq!(r.read(0, &mut buf).unwrap().unreadable.len(), 40);
    }

    #[test]
    fn vanished_device_and_out_of_bounds_are_errors() {
        let (_, r) = reader(faulty(4096).gone_after(0), ReadPolicy::default());
        let mut buf = vec![0; 512];
        assert!(matches!(r.read(0, &mut buf), Err(GuardError::Gone(_))));
        let (_, r) = reader(faulty(4096), ReadPolicy::default());
        assert!(matches!(r.read(4000, &mut buf), Err(GuardError::OutOfBounds)));
        assert!(matches!(
            r.read(u64::MAX - 10, &mut buf),
            Err(GuardError::OutOfBounds)
        ));
    }

    #[test]
    fn stall_is_visible_while_a_read_is_outstanding() {
        let src = faulty(4096).slow(0..1, Duration::from_millis(400));
        let (_, r) = reader(src, ReadPolicy::default());
        let r = Arc::new(r);
        assert!(r.stalled_for().is_none());
        let worker = {
            let r = r.clone();
            std::thread::spawn(move || {
                let mut buf = vec![0; 512];
                r.read(0, &mut buf).unwrap();
            })
        };
        std::thread::sleep(Duration::from_millis(150));
        assert!(r.stalled_for().is_some_and(|d| d >= Duration::from_millis(100)));
        worker.join().unwrap();
        assert!(r.stalled_for().is_none());
    }
}
