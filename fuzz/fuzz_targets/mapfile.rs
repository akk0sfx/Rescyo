//! Any text as a ddrescue mapfile: parsing must never panic, and a parsed map must round-trip
//! and stay consistent under edits.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rescyo_io::mapfile::{BlockStatus, Mapfile};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let Ok(mut m) = Mapfile::parse(text) else { return };
    let again = Mapfile::parse(&m.to_text("fuzz")).expect("a written map parses");
    assert_eq!(again.blocks().collect::<Vec<_>>(), m.blocks().collect::<Vec<_>>());
    let size = m.size();
    let _ = m.bad_map(512);
    if size > 0 {
        let a = u64::from_le_bytes(data.get(..8).and_then(|b| b.try_into().ok()).unwrap_or([0; 8])) % size;
        m.set(a..size.min(a + 4096), BlockStatus::Bad);
        let total: u64 = [
            BlockStatus::NonTried,
            BlockStatus::NonTrimmed,
            BlockStatus::NonScraped,
            BlockStatus::Bad,
            BlockStatus::Finished,
        ]
        .into_iter()
        .map(|s| m.bytes(s))
        .sum();
        assert_eq!(total, size);
    }
});
