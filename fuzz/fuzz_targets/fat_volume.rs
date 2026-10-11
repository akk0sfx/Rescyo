//! A hostile image end to end: probe, deep scan, inference and walk must terminate without
//! panicking, whatever the bytes claim.
#![no_main]

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use rescyo_fat::{WalkOptions, deep_scan, infer_volumes, probe_boot, seeds_for, walk};
use rescyo_fs::{CancelToken, Limits, VecSink};
use rescyo_io::testing::MemSource;
use rescyo_io::{GuardedReader, ReadPolicy};

fuzz_target!(|data: &[u8]| {
    let mut img = data.to_vec();
    img.resize(img.len().div_ceil(512).clamp(8, 4096) * 512, 0);
    let r = GuardedReader::new(Arc::new(MemSource::new(img, 512)), ReadPolicy::default());
    let cancel = CancelToken::new();
    let limits = Limits { max_entries: 10_000, max_dir_bytes: 1 << 20, ..Limits::default() };
    let mut sink = VecSink::default();
    let Ok(scan) = deep_scan(&r, 0..r.geometry().size, &mut sink, &cancel) else { return };
    let mut found = probe_boot(&r, 0).unwrap_or_default();
    found.extend(infer_volumes(&r, &scan).unwrap_or_default());
    for f in found.iter().take(3) {
        let opts = WalkOptions { seeds: seeds_for(&f.geometry, &scan), ..WalkOptions::default() };
        let _ = walk(&r, &f.geometry, &opts, &limits, &mut sink, &cancel);
    }
});
