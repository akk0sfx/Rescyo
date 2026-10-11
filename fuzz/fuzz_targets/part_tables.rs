//! Any byte string as a disk: reading its partition table must never panic or hang.
#![no_main]

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use rescyo_io::testing::MemSource;
use rescyo_io::{GuardedReader, ReadPolicy};

fuzz_target!(|data: &[u8]| {
    let mut img = data.to_vec();
    img.resize(img.len().div_ceil(512).max(4) * 512, 0);
    let r = GuardedReader::new(Arc::new(MemSource::new(img, 512)), ReadPolicy::default());
    let _ = rescyo_part::read_partitions(&r);
});
