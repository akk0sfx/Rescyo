//! A hostile image end to end: scan, inference and walk must terminate without panicking.
#![no_main]

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use rescyo_fs::{CancelToken, Limits, VecSink};
use rescyo_io::testing::MemSource;
use rescyo_io::{GuardedReader, ReadPolicy};

fuzz_target!(|data: &[u8]| {
    let mut img = data.to_vec();
    img.resize(img.len().div_ceil(512).clamp(8, 4096) * 512, 0);
    let r = GuardedReader::new(Arc::new(MemSource::new(img, 512)), ReadPolicy::default());
    let cancel = CancelToken::new();
    let limits = Limits { max_entries: 10_000, ..Limits::default() };
    let mut sink = VecSink::default();
    let Ok(scan) = rescyo_ntfs::deep_scan(&r, 0..r.geometry().size, &mut sink, &cancel) else { return };
    let mut found = rescyo_ntfs::probe_boot(&r, 0, Some(r.geometry().size)).unwrap_or_default();
    found.extend(rescyo_ntfs::infer_volumes(&r, &scan));
    for f in found.iter().take(3) {
        let _ = rescyo_ntfs::walk(&r, &f.geometry, Some(&scan), &limits, &mut sink, &cancel);
    }
});
