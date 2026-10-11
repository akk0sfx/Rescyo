//! Partition search on every corpus scenario and damage combination: a volume is found exactly
//! when one of its boot sectors and one of its tables (FAT or MFT) survive, at the right offset
//! and with the right relation to the partition table.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration test"
)]

use rescyo_core::{PartitionStatus, find_partitions};
use rescyo_fs::CancelToken;
use rescyo_io::{GuardedReader, ReadPolicy};
use rescyo_testkit::require_corpus;

#[test]
fn partition_search_finds_every_volume_that_can_be_found() {
    let _ = require_corpus!();
    let mut lines = Vec::new();
    for s in rescyo_testkit::scenarios() {
        let e = &s.expected;
        for corruption in &e.corruptions {
            let tokens: Vec<&str> = corruption.split('+').collect();
            let has = |t: &str| tokens.contains(&t);
            let primary = !has("boot") && !has("boot_and_backup");
            let backup = matches!(e.fs.as_str(), "fat32" | "exfat" | "ntfs")
                && !has("backup_boot")
                && !has("boot_and_backup");
            // exFAT checks itself by its boot-region checksum; FAT needs one surviving table.
            let table_ok = e.fs == "exfat" || e.fs == "ntfs" || !(has("fat1") && has("fat2"));
            let findable = (primary || backup) && table_ok;
            let r = GuardedReader::new(s.open(corruption).unwrap(), ReadPolicy::default());
            let rep = find_partitions(&r, &mut |_| {}, &CancelToken::new()).unwrap();
            let main: Vec<_> = rep
                .found
                .iter()
                .filter(|f| f.offset == e.partition.offset && f.status != PartitionStatus::Nested)
                .collect();
            let tag = format!("{} {corruption}", e.scenario);
            if !findable {
                assert!(main.is_empty(), "{tag}: found {main:?}");
                lines.push(format!("{tag:<40} not findable, not found"));
                continue;
            }
            assert_eq!(main.len(), 1, "{tag}: {:?}", rep.found);
            let f = main[0];
            assert_eq!(f.fs.as_str(), e.fs, "{tag}");
            assert!(
                f.size <= e.partition.size && f.size + (1 << 20) > e.partition.size,
                "{tag}: size {} vs partition {}",
                f.size,
                e.partition.size
            );
            assert_eq!((f.primary_boot, f.backup_boot), (primary, backup), "{tag}");
            let want = if e.table == "none" || has("mbr") {
                if e.partition.offset == 0 {
                    PartitionStatus::Unpartitioned
                } else {
                    PartitionStatus::Lost
                }
            } else {
                PartitionStatus::Listed
            };
            assert_eq!(f.status, want, "{tag}");
            // Nothing else claims to be a lost partition.
            let others: Vec<_> = rep
                .found
                .iter()
                .filter(|x| x.offset != e.partition.offset && x.status == PartitionStatus::Lost)
                .collect();
            assert!(others.is_empty(), "{tag}: spurious {others:?}");
            lines.push(format!("{tag:<40} {:?} at {:#x}", f.status, f.offset));
        }
    }
    eprintln!("{}", lines.join("\n"));
}
