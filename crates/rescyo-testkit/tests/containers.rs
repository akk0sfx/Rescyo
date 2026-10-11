//! Every container written by the reference tools (`cargo xtask containers`: qemu-img and
//! ewfacquire) reads back as the raw corpus image, byte for byte.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration test"
)]

use rescyo_io::{ContainerFormat, open_image};
use serde::Deserialize;

#[derive(Deserialize)]
struct Manifest {
    size: u64,
    containers: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    file: String,
    format: String,
    /// Ranges written after wrapping: (offset, length, byte value).
    #[serde(default)]
    overwritten: Vec<(u64, u64, u8)>,
}

#[test]
fn reference_containers_read_back_as_the_raw_image() {
    let mut checked = 0;
    for s in rescyo_testkit::require_corpus!() {
        let dir = s.image_path().parent().unwrap().join("containers");
        let Ok(text) = std::fs::read_to_string(dir.join("containers.json")) else {
            continue;
        };
        let m: Manifest = serde_json::from_str(&text).unwrap();
        let raw = std::fs::read(s.image_path()).unwrap();
        assert_eq!(raw.len() as u64, m.size);
        for c in &m.containers {
            let name = format!("{}/{}", s.expected.scenario, c.file);
            let (src, info) =
                open_image(&dir.join(&c.file), 512, None).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                info.format,
                ContainerFormat::from_name(&c.format).unwrap(),
                "{name}"
            );
            assert!(info.notes.is_empty(), "{name}: {:?}", info.notes);
            let g = src.geometry();
            // VHD rounds the virtual size up to its CHS geometry unless force_size was used;
            // the image's own bytes must match and anything past them must be zero.
            assert!(g.size >= m.size, "{name}: {} < {}", g.size, m.size);
            let mut expected = raw.clone();
            for &(at, len, v) in &c.overwritten {
                let at = usize::try_from(at).unwrap();
                expected[at..at + usize::try_from(len).unwrap()].fill(v);
            }
            let mut buf = vec![0u8; 1 << 20];
            let mut off = 0u64;
            while off < g.size {
                let n = usize::try_from((g.size - off).min(buf.len() as u64)).unwrap();
                src.read_at(off, &mut buf[..n])
                    .unwrap_or_else(|e| panic!("{name} @ {off}: {e}"));
                let lo = usize::try_from(off).unwrap();
                let want = &expected[lo.min(expected.len())..(lo + n).min(expected.len())];
                assert!(
                    buf[..want.len()] == *want,
                    "{name}: differs within {off}..{}",
                    off + n as u64
                );
                assert!(
                    buf[want.len()..n].iter().all(|&b| b == 0),
                    "{name}: non-zero tail"
                );
                off += n as u64;
            }
            checked += 1;
        }
    }
    eprintln!("checked {checked} reference containers");
    if checked == 0 {
        eprintln!("skipped: no containers (run `cargo xtask containers`)");
    }
}
