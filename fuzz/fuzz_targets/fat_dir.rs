//! Directory bytes from a hostile disk: FAT and exFAT entry parsers must never panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rescyo_fat::dir::{DirContext, looks_like_dir, parse_dir};
use rescyo_fat::exdir::{ExContext, looks_like_exdir, parse_exdir};
use rescyo_fs::codepage::Codepage;

fuzz_target!(|data: &[u8]| {
    for cp in [Codepage::Cp437, Codepage::Cp866, Codepage::Cp932] {
        let ctx = DirContext { fat32: data.first().is_some_and(|b| b & 1 == 1), max_cluster: 100_000, max_size: 1 << 32, codepage: cp };
        let _ = parse_dir(data, &ctx, 4096);
        let _ = looks_like_dir(data, &ctx, true);
    }
    let ex = ExContext { max_cluster: 100_000, cluster_size: 4096, max_size: 1 << 40 };
    let _ = parse_exdir(data, &ex, 4096);
    let _ = looks_like_exdir(data, &ex, false);
});
