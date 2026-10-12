//! A hostile ext2/3/4 image end to end: probe, walk and read every file's layout without
//! panicking or looping.
#![no_main]

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use rescyo_fs::{CancelToken, Limits, VecSink};
use rescyo_io::testing::MemSource;
use rescyo_io::{GuardedReader, ReadPolicy};

fuzz_target!(|data: &[u8]| {
    let mut img = data.to_vec();
    img.resize(img.len().div_ceil(512).clamp(8, 8192) * 512, 0);
    let r = GuardedReader::new(Arc::new(MemSource::new(img, 512)), ReadPolicy::default());
    let cancel = CancelToken::new();
    let limits = Limits { max_entries: 10_000, max_extents: 4096, ..Limits::default() };
    for f in rescyo_ext4::probe(&r, 0).unwrap_or_default() {
        let mut sink = VecSink::default();
        let _ = rescyo_ext4::walk(&r, &f.geometry, &limits, &mut sink, &cancel);
        for e in &sink.entries {
            let _ = rescyo_fs::content::stream(&r, &e.layout, e.size.min(1 << 20), &mut |_, _| Ok(()));
        }
    }
});
