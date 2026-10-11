//! One hostile MFT record: fixup, header, attributes, runlists and names must never panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rescyo_ntfs::record::{self, AttrBody};

fuzz_target!(|data: &[u8]| {
    let mut rec = data.to_vec();
    rec.resize(1024, 0);
    let _ = record::apply_fixup(&mut rec, 512);
    let Some(h) = record::header(&rec) else { return };
    for a in record::attributes(&rec, &h) {
        match a.body {
            AttrBody::Resident(v) => {
                let _ = record::file_name(v);
                let _ = record::standard_times(v);
            }
            AttrBody::NonResident { runs, .. } => {
                let _ = record::decode_runs(runs, 4096, 1 << 40);
            }
        }
    }
});
