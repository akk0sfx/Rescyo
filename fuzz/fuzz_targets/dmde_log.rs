//! Any bytes as a DMDE scan log: parsing and seed extraction must never panic.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = rescyo_dmde::parse(data) {
        for v in &s.fat_volumes {
            let _ = s.seeds_for(v);
        }
        let _ = s.best_volume();
    }
});
