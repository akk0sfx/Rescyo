//! Any bytes as a container image (VHD, VHDX, VMDK, qcow2, E01, chosen by the first byte):
//! opening and reading the virtual disk must never panic, hang or allocate without bound.
#![no_main]

use std::io::Write;

use libfuzzer_sys::fuzz_target;
use rescyo_io::{ContainerFormat, open_image};

const FORMATS: [ContainerFormat; 5] = [
    ContainerFormat::Vhd,
    ContainerFormat::Vhdx,
    ContainerFormat::Vmdk,
    ContainerFormat::Qcow2,
    ContainerFormat::Ewf,
];

fuzz_target!(|data: &[u8]| {
    let Some((&pick, body)) = data.split_first() else { return };
    let format = FORMATS[usize::from(pick) % FORMATS.len()];
    let Ok(dir) = tempfile::tempdir() else { return };
    let path = dir.path().join("image");
    let Ok(mut f) = std::fs::File::create(&path) else { return };
    if f.write_all(body).is_err() {
        return;
    }
    drop(f);
    let Ok((src, _)) = open_image(&path, 512, Some(format)) else { return };
    let size = src.geometry().size;
    let sector = u64::from(src.geometry().logical_sector);
    // Read up to 64 sectors spread over the virtual disk.
    let mut buf = vec![0u8; usize::try_from(sector).unwrap_or(512)];
    let sectors = size / sector;
    if sectors == 0 {
        return;
    }
    for i in 0..64u64.min(sectors) {
        let s = i * (sectors / 64).max(1);
        let _ = src.read_at(s * sector, &mut buf);
    }
});
