//! Any bytes as a disk to carve: following hostile structures must never panic, loop forever or
//! report a file reaching past the disk.
#![no_main]

use std::collections::HashSet;
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use rescyo_fs::CancelToken;
use rescyo_io::testing::MemSource;
use rescyo_io::{GuardedReader, ReadPolicy};

fuzz_target!(|data: &[u8]| {
    let mut disk = data.to_vec();
    disk.resize(data.len().div_ceil(512).max(1) * 512, 0);
    let size = disk.len() as u64;
    let r = GuardedReader::new(Arc::new(MemSource::new(disk.clone(), 512)), ReadPolicy::default());
    let mut c = rescyo_carve::Carver::new();
    c.feed(0, &disk);
    if let Ok(files) = c.finish(&r, &HashSet::new(), &CancelToken::new()) {
        for f in files {
            assert!(f.offset + f.len <= size);
        }
    }
});
