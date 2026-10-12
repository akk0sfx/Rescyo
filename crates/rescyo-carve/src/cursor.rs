//! Buffered random access to the bytes of one candidate file, bounded by its size cap.

use rescyo_io::{GuardError, GuardedReader};

const PAGE: u64 = 64 << 10;

pub struct Cursor<'a> {
    r: &'a GuardedReader,
    /// Absolute offset of the candidate's first byte.
    base: u64,
    /// Bytes the candidate may span (its size cap, clipped to the Source).
    limit: u64,
    page: Vec<u8>,
    page_at: u64,
    /// Sticky reader failure (alarm, source gone), surfaced by the caller.
    pub failed: Option<GuardError>,
}

impl<'a> Cursor<'a> {
    pub fn new(r: &'a GuardedReader, base: u64, limit: u64) -> Self {
        let limit = limit.min(r.geometry().size.saturating_sub(base));
        Self {
            r,
            base,
            limit,
            page: Vec::new(),
            page_at: u64::MAX,
            failed: None,
        }
    }

    fn load(&mut self, pos: u64) -> Option<()> {
        if self.failed.is_some() || pos >= self.limit {
            return None;
        }
        let at = pos - pos % PAGE;
        if at == self.page_at {
            return Some(());
        }
        let n = PAGE.min(self.limit - at);
        // Reads stay sector-aligned: round the page up, then keep only what is in bounds.
        let ss = u64::from(self.r.geometry().logical_sector).max(1);
        let abs = self.base + at;
        let start = abs - abs % ss;
        let end = (abs + n).div_ceil(ss) * ss;
        let end = end.min(self.r.geometry().size);
        let mut buf = vec![0u8; usize::try_from(end - start).ok()?];
        match self.r.read(start, &mut buf) {
            Ok(_) => {}
            Err(GuardError::OutOfBounds) => return None,
            Err(e) => {
                self.failed = Some(e);
                return None;
            }
        }
        let skip = usize::try_from(abs - start).ok()?;
        self.page = buf.get(skip..skip + usize::try_from(n).ok()?)?.to_vec();
        self.page_at = at;
        Some(())
    }

    pub fn byte(&mut self, pos: u64) -> Option<u8> {
        self.load(pos)?;
        self.page.get(usize::try_from(pos - self.page_at).ok()?).copied()
    }

    pub fn bytes(&mut self, pos: u64, n: usize) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(n);
        let mut p = pos;
        while out.len() < n {
            self.load(p)?;
            let i = usize::try_from(p - self.page_at).ok()?;
            let take = (n - out.len()).min(self.page.len().saturating_sub(i));
            if take == 0 {
                return None;
            }
            out.extend_from_slice(self.page.get(i..i + take)?);
            p += take as u64;
        }
        Some(out)
    }

    /// Position of the next `b` at or after `pos`.
    pub fn find_byte(&mut self, mut pos: u64, b: u8) -> Option<u64> {
        loop {
            self.load(pos)?;
            let i = usize::try_from(pos - self.page_at).ok()?;
            if let Some(k) = self.page.get(i..)?.iter().position(|&x| x == b) {
                return Some(pos + k as u64);
            }
            pos = self.page_at + self.page.len() as u64;
        }
    }

    /// Position of the next `needle` at or after `pos`.
    pub fn find(&mut self, mut pos: u64, needle: &[u8]) -> Option<u64> {
        let first = *needle.first()?;
        loop {
            let at = self.find_byte(pos, first)?;
            if self.bytes(at, needle.len())? == needle {
                return Some(at);
            }
            pos = at + 1;
        }
    }
}
