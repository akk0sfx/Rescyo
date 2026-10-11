//! Imaging: copy a failing Source to an image file first, recover from the image afterwards.
//!
//! The passes follow GNU ddrescue's strategy, and the map is a ddrescue mapfile, so a copy can
//! be continued by either tool:
//!
//! 1. **Copy**: large blocks forward; on an error the block is marked non-trimmed and the copy
//!    jumps ahead by a growing distance, getting the easy data off the disk first.
//! 2. **Copy again**: the areas jumped over, without jumping (backwards when asked).
//! 3. **Trim**: each failed block is read sector by sector from both edges until the first
//!    error; what is left in the middle is non-scraped.
//! 4. **Scrape**: every remaining sector once.
//! 5. **Retry**: bad sectors again, only if asked (`retries`).
//!
//! The image is synced before every map save, so the map never claims data the image lacks.
//! The map is written atomically (temporary file, then rename).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rescyo_fs::CancelToken;
use rescyo_io::mapfile::{BlockStatus, Mapfile, MapfileError};
use rescyo_io::{BlockSource, ReadError};
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct ImageOptions {
    /// Bytes read at once during the copy passes.
    pub block: u64,
    /// Extra attempts for each bad sector after scraping.
    pub retries: u32,
    /// Second copy pass and scraping run backwards (helps some drives).
    pub reverse: bool,
    /// How often the map is saved during long passes.
    pub save_every: Duration,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            block: 1 << 20,
            retries: 0,
            reverse: true,
            save_every: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImagePhase {
    Copy,
    Trim,
    Scrape,
    Retry,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImageProgress {
    pub phase: ImagePhase,
    pub pos: u64,
    pub size: u64,
    pub rescued: u64,
    pub bad: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageReport {
    pub size: u64,
    pub rescued: u64,
    /// Bytes in bad sectors.
    pub bad_bytes: u64,
    /// Separate bad areas.
    pub bad_areas: u64,
    /// Bytes not tried, not trimmed or not scraped (an interrupted copy).
    pub pending: u64,
    /// Every byte is either rescued or a confirmed bad sector.
    pub finished: bool,
    /// The run continued an earlier map.
    pub resumed: bool,
    pub map: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ImagingError {
    #[error("{0} already exists and there is no map to resume from")]
    OutputExists(PathBuf),
    #[error("the map describes {map} bytes but the source has {source_size}")]
    SizeMismatch { map: u64, source_size: u64 },
    #[error("cannot read the map: {0}")]
    Map(#[from] MapfileError),
    #[error("cannot write the image or map: {0}")]
    Io(#[from] io::Error),
    #[error("the source disappeared: {0}")]
    Gone(io::Error),
    #[error("stopped; the map is saved and the copy can be resumed")]
    Cancelled(ImageReport),
}

struct Imager<'a> {
    src: &'a dyn BlockSource,
    out: File,
    map: Mapfile,
    map_path: PathBuf,
    ss: u64,
    opts: &'a ImageOptions,
    progress: &'a mut dyn FnMut(ImageProgress),
    cancel: &'a CancelToken,
    last_save: Instant,
    buf: Vec<u8>,
}

/// Copies `src` to `out`, tracking progress in `map_path` (created, or resumed if it exists).
pub fn image(
    src: &dyn BlockSource,
    out: &Path,
    map_path: &Path,
    opts: &ImageOptions,
    progress: &mut dyn FnMut(ImageProgress),
    cancel: &CancelToken,
) -> Result<ImageReport, ImagingError> {
    let g = src.geometry();
    let ss = u64::from(g.logical_sector);
    let (map, file, resumed) = if map_path.exists() {
        let mut map = Mapfile::parse(&std::fs::read_to_string(map_path)?)?;
        if map.size() != g.size && map.size() != 0 {
            return Err(ImagingError::SizeMismatch {
                map: map.size(),
                source_size: g.size,
            });
        }
        if map.size() == 0 {
            map = Mapfile::new(g.size);
        }
        // Without the image, rescued data in the map would be a lie: start over.
        let file = if out.exists() {
            OpenOptions::new().write(true).open(out)?
        } else {
            map = Mapfile::new(g.size);
            File::create_new(out)?
        };
        (map, file, true)
    } else {
        if out.exists() {
            return Err(ImagingError::OutputExists(out.to_path_buf()));
        }
        (Mapfile::new(g.size), File::create_new(out)?, false)
    };
    file.set_len(g.size)?;
    let block = (opts.block.max(ss) / ss) * ss;
    let mut im = Imager {
        src,
        out: file,
        map,
        map_path: map_path.to_path_buf(),
        ss,
        opts,
        progress,
        cancel,
        last_save: Instant::now(),
        buf: vec![0u8; usize::try_from(block).unwrap_or(1 << 20)],
    };
    im.save()?;
    let result = im.run(block);
    // Always leave a consistent map behind, whatever happened.
    im.save()?;
    let report = im.report(resumed);
    match result {
        Ok(()) => Ok(report),
        Err(Stop::Cancelled) => Err(ImagingError::Cancelled(report)),
        Err(Stop::Gone(e)) => Err(ImagingError::Gone(e)),
        Err(Stop::Io(e)) => Err(ImagingError::Io(e)),
    }
}

enum Stop {
    Cancelled,
    Gone(io::Error),
    Io(io::Error),
}

impl From<io::Error> for Stop {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Outcome of one read.
enum Got {
    Data,
    Bad,
}

impl Imager<'_> {
    fn run(&mut self, block: u64) -> Result<(), Stop> {
        // Pass 1: forward, jumping over trouble.
        self.map.current_status = '?';
        self.map.current_pass = 1;
        self.copy(block, true, false)?;
        // Pass 2: what was jumped over, without jumping.
        self.map.current_pass = 2;
        self.copy(block, false, self.opts.reverse)?;
        self.map.current_status = '*';
        self.trim()?;
        self.map.current_status = '/';
        self.scrape()?;
        self.map.current_status = '-';
        for _ in 0..self.opts.retries {
            self.retry()?;
        }
        self.map.current_status = '+';
        self.report_progress(ImagePhase::Done, self.map.size());
        Ok(())
    }

    fn read(&mut self, at: u64, n: u64) -> Result<Got, Stop> {
        let len = usize::try_from(n).unwrap_or(0);
        let buf = self.buf.get_mut(..len).unwrap_or_default();
        match self.src.read_at(at, buf) {
            Ok(()) => {
                write_at(&self.out, at, buf)?;
                Ok(Got::Data)
            }
            Err(ReadError::Gone(e)) => Err(Stop::Gone(e)),
            Err(ReadError::Media | ReadError::OutOfBounds | ReadError::Misaligned) => Ok(Got::Bad),
            Err(ReadError::Io(_)) => Ok(Got::Bad),
        }
    }

    fn tick(&mut self, phase: ImagePhase, pos: u64) -> Result<(), Stop> {
        if self.cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        self.map.current_pos = pos;
        if self.last_save.elapsed() >= self.opts.save_every {
            self.save()?;
        }
        self.report_progress(phase, pos);
        Ok(())
    }

    fn report_progress(&mut self, phase: ImagePhase, pos: u64) {
        let p = ImageProgress {
            phase,
            pos,
            size: self.map.size(),
            rescued: self.map.bytes(BlockStatus::Finished),
            bad: self.map.bytes(BlockStatus::Bad),
        };
        (self.progress)(p);
    }

    fn copy(&mut self, block: u64, skipping: bool, backwards: bool) -> Result<(), Stop> {
        let mut todo = self.map.ranges(BlockStatus::NonTried);
        if backwards {
            todo.reverse();
        }
        for r in todo {
            let mut jump = 0u64;
            let max_jump = (self.map.size() / 100).clamp(block, 1 << 30);
            let mut pos = if backwards { r.end } else { r.start };
            loop {
                let (at, n) = if backwards {
                    if pos <= r.start {
                        break;
                    }
                    let n = block.min(pos - r.start);
                    (pos - n, n)
                } else {
                    if pos >= r.end {
                        break;
                    }
                    (pos, block.min(r.end - pos))
                };
                // A block reaching into an area another pass already settled is cut short.
                if self.map.status_at(at) != Some(BlockStatus::NonTried) {
                    pos = if backwards { at } else { at + n };
                    continue;
                }
                self.tick(ImagePhase::Copy, at)?;
                match self.read(at, n)? {
                    Got::Data => {
                        self.map.set(at..at + n, BlockStatus::Finished);
                        jump = 0;
                    }
                    Got::Bad => {
                        self.map.set(at..at + n, BlockStatus::NonTrimmed);
                        if skipping {
                            jump = (jump * 2).clamp(block, max_jump);
                            pos = if backwards {
                                at.saturating_sub(jump)
                            } else {
                                at + n + jump
                            };
                            continue;
                        }
                    }
                }
                pos = if backwards { at } else { at + n };
            }
        }
        Ok(())
    }

    fn sector(&mut self, at: u64, phase: ImagePhase) -> Result<bool, Stop> {
        self.tick(phase, at)?;
        let ss = self.ss;
        Ok(match self.read(at, ss)? {
            Got::Data => {
                self.map.set(at..at + ss, BlockStatus::Finished);
                true
            }
            Got::Bad => {
                self.map.set(at..at + ss, BlockStatus::Bad);
                false
            }
        })
    }

    fn trim(&mut self) -> Result<(), Stop> {
        for r in self.map.ranges(BlockStatus::NonTrimmed) {
            let mut lo = r.start;
            while lo < r.end && self.sector(lo, ImagePhase::Trim)? {
                lo += self.ss;
            }
            if lo < r.end {
                lo += self.ss; // the failing sector is already marked bad
            }
            let mut hi = r.end;
            while hi > lo && self.sector(hi - self.ss, ImagePhase::Trim)? {
                hi -= self.ss;
            }
            if hi > lo {
                hi -= self.ss;
            }
            if hi > lo {
                self.map.set(lo..hi, BlockStatus::NonScraped);
            }
        }
        Ok(())
    }

    fn scrape(&mut self) -> Result<(), Stop> {
        let mut todo = self.map.ranges(BlockStatus::NonScraped);
        if self.opts.reverse {
            todo.reverse();
        }
        for r in todo {
            let sectors: Vec<u64> = (r.start..r.end)
                .step_by(usize::try_from(self.ss).unwrap_or(512))
                .collect();
            let order: Box<dyn Iterator<Item = &u64>> = if self.opts.reverse {
                Box::new(sectors.iter().rev())
            } else {
                Box::new(sectors.iter())
            };
            for &at in order {
                self.sector(at, ImagePhase::Scrape)?;
            }
        }
        Ok(())
    }

    fn retry(&mut self) -> Result<(), Stop> {
        for r in self.map.ranges(BlockStatus::Bad) {
            let mut at = r.start;
            while at < r.end {
                self.sector(at, ImagePhase::Retry)?;
                at += self.ss;
            }
        }
        Ok(())
    }

    /// Syncs the image, then replaces the map atomically.
    fn save(&mut self) -> io::Result<()> {
        self.out.sync_data()?;
        let tmp = self.map_path.with_extension("map.tmp");
        std::fs::write(
            &tmp,
            self.map.to_text(concat!("Rescyo ", env!("CARGO_PKG_VERSION"))),
        )?;
        std::fs::rename(&tmp, &self.map_path)?;
        self.last_save = Instant::now();
        Ok(())
    }

    fn report(&self, resumed: bool) -> ImageReport {
        let pending = self.map.bytes(BlockStatus::NonTried)
            + self.map.bytes(BlockStatus::NonTrimmed)
            + self.map.bytes(BlockStatus::NonScraped);
        ImageReport {
            size: self.map.size(),
            rescued: self.map.bytes(BlockStatus::Finished),
            bad_bytes: self.map.bytes(BlockStatus::Bad),
            bad_areas: self.map.ranges(BlockStatus::Bad).len() as u64,
            pending,
            finished: pending == 0,
            resumed,
            map: self.map_path.display().to_string(),
        }
    }
}

#[cfg(unix)]
fn write_at(f: &File, at: u64, buf: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.write_all_at(buf, at)
}

#[cfg(windows)]
fn write_at(f: &File, mut at: u64, mut buf: &[u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, at)?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buf = buf.get(n..).unwrap_or_default();
        at += n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rescyo_io::testing::{FaultySource, MemSource, pattern};

    use super::*;

    const SIZE: usize = 4 << 20;

    fn faulty() -> FaultySource<MemSource> {
        // Two damaged areas: a single sector and a 40-sector band.
        FaultySource::new(MemSource::new(pattern(SIZE), 512))
            .bad(100..101)
            .bad(3000..3040)
    }

    fn opts() -> ImageOptions {
        ImageOptions {
            block: 64 << 10,
            retries: 0,
            reverse: true,
            save_every: Duration::ZERO,
        }
    }

    #[test]
    fn images_everything_readable_and_maps_exactly_the_bad_sectors() {
        let dir = tempfile::tempdir().unwrap();
        let (out, map) = (dir.path().join("d.img"), dir.path().join("d.map"));
        let src = faulty();
        let rep = image(&src, &out, &map, &opts(), &mut |_| {}, &CancelToken::new()).unwrap();
        assert!(rep.finished);
        assert_eq!(rep.bad_bytes, 41 * 512);
        assert_eq!(rep.bad_areas, 2);
        let img = std::fs::read(&out).unwrap();
        let src_bytes = pattern(SIZE);
        for s in 0..SIZE / 512 {
            let bad = s == 100 || (3000..3040).contains(&s);
            let part = &img[s * 512..(s + 1) * 512];
            if bad {
                assert!(part.iter().all(|&b| b == 0), "sector {s}");
            } else {
                assert_eq!(part, &src_bytes[s * 512..(s + 1) * 512], "sector {s}");
            }
        }
        let m = Mapfile::parse(&std::fs::read_to_string(&map).unwrap()).unwrap();
        assert_eq!(
            m.ranges(BlockStatus::Bad),
            vec![100 * 512..101 * 512, 3000 * 512..3040 * 512]
        );
        assert_eq!(m.current_status, '+');
        // Each bad sector was read at most a few times (block read, trim and/or scrape), never
        // hammered.
        assert!(
            (0..SIZE as u64 / 512).all(|s| src.reads_of(s) <= 4),
            "excessive rereads"
        );
    }

    #[test]
    fn an_interrupted_copy_resumes_from_its_map() {
        let dir = tempfile::tempdir().unwrap();
        let (out, map) = (dir.path().join("d.img"), dir.path().join("d.map"));
        let src = Arc::new(faulty());
        let cancel = CancelToken::new();
        let stopper = cancel.clone();
        let err = image(
            src.as_ref(),
            &out,
            &map,
            &opts(),
            &mut |p| {
                if p.rescued > (1 << 20) {
                    stopper.cancel();
                }
            },
            &cancel,
        )
        .unwrap_err();
        let ImagingError::Cancelled(partial) = err else {
            panic!("expected cancel")
        };
        assert!(!partial.finished && partial.rescued >= 1 << 20);
        let done = Mapfile::parse(&std::fs::read_to_string(&map).unwrap()).unwrap();
        let rescued_before: Vec<u64> = (0..SIZE as u64 / 512)
            .filter(|s| done.status_at(s * 512) == Some(BlockStatus::Finished))
            .collect();
        let reads_before: Vec<u64> = rescued_before.iter().map(|&s| src.reads_of(s)).collect();
        let rep = image(
            src.as_ref(),
            &out,
            &map,
            &opts(),
            &mut |_| {},
            &CancelToken::new(),
        )
        .unwrap();
        assert!(rep.finished && rep.resumed);
        assert_eq!(rep.rescued, (SIZE as u64) - 41 * 512);
        // Nothing rescued before the stop was read again.
        let reads_after: Vec<u64> = rescued_before.iter().map(|&s| src.reads_of(s)).collect();
        assert_eq!(reads_before, reads_after);
        assert_eq!(std::fs::read(&out).unwrap()[..512], pattern(SIZE)[..512]);
    }

    #[test]
    fn refuses_to_overwrite_an_image_without_a_map() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("d.img");
        std::fs::write(&out, b"precious").unwrap();
        let err = image(
            &faulty(),
            &out,
            &dir.path().join("d.map"),
            &opts(),
            &mut |_| {},
            &CancelToken::new(),
        );
        assert!(matches!(err, Err(ImagingError::OutputExists(_))));
        assert_eq!(std::fs::read(&out).unwrap(), b"precious");
    }

    #[test]
    fn a_vanished_source_leaves_a_resumable_map() {
        let dir = tempfile::tempdir().unwrap();
        let (out, map) = (dir.path().join("d.img"), dir.path().join("d.map"));
        let src = FaultySource::new(MemSource::new(pattern(SIZE), 512)).gone_after(10);
        let err = image(&src, &out, &map, &opts(), &mut |_| {}, &CancelToken::new());
        assert!(matches!(err, Err(ImagingError::Gone(_))));
        let m = Mapfile::parse(&std::fs::read_to_string(&map).unwrap()).unwrap();
        assert_eq!(m.bytes(BlockStatus::Finished), 10 * (64 << 10));
    }
}
