//! Carving over every corpus scenario: each contiguous JPEG, DOCX and XLSX file the scenario
//! wrote (live or deleted) is carved byte for byte, with its extension, without any help from
//! the file system.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "integration test"
)]

use std::collections::{HashMap, HashSet};

use rescyo_fs::CancelToken;
use rescyo_io::{GuardedReader, ReadPolicy};
use rescyo_testkit::require_corpus;
use sha2::{Digest, Sha256};

#[test]
fn carving_recovers_every_contiguous_document_and_photo() {
    let _ = require_corpus!();
    let mut total = 0;
    for s in rescyo_testkit::scenarios() {
        let e = &s.expected;
        let r = GuardedReader::new(s.open("none").unwrap(), ReadPolicy::default());
        let size = r.geometry().size;
        let mut c = rescyo_carve::Carver::new();
        let mut buf = vec![0u8; 4 << 20];
        let mut at = 0;
        while at < size {
            let n = (size - at).min(buf.len() as u64) as usize;
            r.read(at, &mut buf[..n]).unwrap();
            c.feed(at, &buf[..n]);
            at += n as u64;
        }
        let carved = c.finish(&r, &HashSet::new(), &CancelToken::new()).unwrap();
        let by_sha: HashMap<String, &str> = carved
            .iter()
            .map(|f| {
                let mut b = vec![0u8; f.len as usize];
                // Reads must be sector-aligned: round out and slice.
                let start = f.offset - f.offset % 512;
                let end = (f.offset + f.len).div_ceil(512) * 512;
                let mut w = vec![0u8; (end - start) as usize];
                r.read(start, &mut w).unwrap();
                b.copy_from_slice(&w[(f.offset - start) as usize..][..f.len as usize]);
                (
                    Sha256::digest(&b).iter().map(|x| format!("{x:02x}")).collect(),
                    f.ext,
                )
            })
            .collect();
        for f in &e.files {
            let ext = f.path.rsplit('.').next().unwrap_or("");
            // Small files may live inside NTFS records, not at a sector start.
            if !matches!(ext, "jpg" | "docx" | "xlsx") || f.fragmented == Some(true) || f.size < 1024 {
                continue;
            }
            let got = by_sha.get(&f.sha256);
            assert_eq!(got, Some(&ext), "{}: {} not carved", e.scenario, f.path);
            total += 1;
        }
    }
    eprintln!("carved and verified: {total}");
    assert!(total > 0);
}
