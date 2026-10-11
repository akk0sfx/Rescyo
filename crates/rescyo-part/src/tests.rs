// Test images are small; byte offsets fit in usize on every supported target.
#![allow(clippy::cast_possible_truncation)]

use std::sync::Arc;

use rescyo_io::testing::MemSource;
use rescyo_io::{GuardedReader, ReadPolicy};

use super::*;

const SS: usize = 512;

fn reader(img: Vec<u8>) -> GuardedReader {
    GuardedReader::new(Arc::new(MemSource::new(img, 512)), ReadPolicy::default())
}

fn put_mbr_entry(sector: &mut [u8], i: usize, kind: u8, start: u32, len: u32) {
    let o = 446 + i * 16;
    sector[o + 4] = kind;
    sector[o + 8..o + 12].copy_from_slice(&start.to_le_bytes());
    sector[o + 12..o + 16].copy_from_slice(&len.to_le_bytes());
    sector[510] = 0x55;
    sector[511] = 0xAA;
}

/// Writes a GPT header at `lba` whose entry array (already in `img`) is at `entries_lba`.
fn put_gpt(img: &mut [u8], lba: u64, alt: u64, entries_lba: u64, count: u32) {
    let total = (img.len() / SS) as u64;
    let array_off = entries_lba as usize * SS;
    let array_crc = crc32fast::hash(&img[array_off..array_off + count as usize * 128]);
    let h = &mut img[lba as usize * SS..lba as usize * SS + SS];
    h.fill(0);
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&lba.to_le_bytes());
    h[32..40].copy_from_slice(&alt.to_le_bytes());
    h[40..48].copy_from_slice(&34u64.to_le_bytes());
    h[48..56].copy_from_slice(&(total - 34).to_le_bytes());
    h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    h[80..84].copy_from_slice(&count.to_le_bytes());
    h[84..88].copy_from_slice(&128u32.to_le_bytes());
    h[88..92].copy_from_slice(&array_crc.to_le_bytes());
    let crc = crc32fast::hash(&h[..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
}

fn gpt_image(sectors: usize) -> Vec<u8> {
    let mut img = vec![0u8; sectors * SS];
    let entries = |img: &mut Vec<u8>, at: usize| {
        let e = &mut img[at * SS..at * SS + 128];
        e[0..16].copy_from_slice(&[
            0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7,
        ]);
        e[16..32].copy_from_slice(&[7; 16]);
        e[32..40].copy_from_slice(&2048u64.to_le_bytes());
        e[40..48].copy_from_slice(&4095u64.to_le_bytes());
        for (i, c) in "Data".encode_utf16().enumerate() {
            e[56 + i * 2..58 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
    };
    entries(&mut img, 2);
    let last = sectors - 1;
    entries(&mut img, last - 32);
    put_gpt(&mut img, 1, last as u64, 2, 128);
    put_gpt(&mut img, last as u64, 1, (last - 32) as u64, 128);
    put_mbr_entry(&mut img[..SS], 0, 0xEE, 1, (sectors - 1) as u32);
    img
}

#[test]
fn reads_primary_mbr_entries() {
    let mut img = vec![0u8; 8192 * SS];
    put_mbr_entry(&mut img[..SS], 0, 0x0C, 2048, 4096);
    put_mbr_entry(&mut img[..SS], 1, 0x07, 6144, 2048);
    let t = read_partitions(&reader(img)).unwrap();
    assert_eq!(t.kind, TableKind::Mbr);
    assert_eq!(t.partitions.len(), 2);
    assert_eq!(
        (t.partitions[0].offset, t.partitions[0].size),
        (2048 * 512, 4096 * 512)
    );
    assert_eq!(t.partitions[1].kind, PartitionType::Mbr(0x07));
    assert!(t.warnings.is_empty());
}

#[test]
fn follows_ebr_chain_and_stops_on_loops() {
    let mut img = vec![0u8; 16384 * SS];
    put_mbr_entry(&mut img[..SS], 0, 0x0F, 1000, 15000);
    // EBR 1 at 1000: logical at +63, next EBR at ext+5000.
    put_mbr_entry(&mut img[1000 * SS..1001 * SS], 0, 0x07, 63, 1000);
    put_mbr_entry(&mut img[1000 * SS..1001 * SS], 1, 0x05, 5000, 2000);
    // EBR 2 at 6000: logical, and a link pointing back to the first EBR (a loop).
    put_mbr_entry(&mut img[6000 * SS..6001 * SS], 0, 0x0B, 63, 500);
    put_mbr_entry(&mut img[6000 * SS..6001 * SS], 1, 0x05, 0, 0);
    let t = read_partitions(&reader(img.clone())).unwrap();
    assert_eq!(
        t.partitions.iter().map(|p| p.number).collect::<Vec<_>>(),
        vec![5, 6]
    );
    assert_eq!(t.partitions[0].offset, 1063 * 512);
    assert_eq!(t.partitions[1].offset, 6063 * 512);

    put_mbr_entry(&mut img[6000 * SS..6001 * SS], 1, 0x05, 1, 10);
    // rel 1 -> EBR at 1001: no signature -> chain broken, but found partitions are kept.
    let t = read_partitions(&reader(img.clone())).unwrap();
    assert_eq!(t.partitions.len(), 2);
    assert!(t.warnings.contains(&TableWarning::EbrChainBroken));

    // A true loop: EBR 2 links to itself.
    let mut looped = img;
    put_mbr_entry(&mut looped[6000 * SS..6001 * SS], 1, 0x05, 5000, 10);
    let t = read_partitions(&reader(looped)).unwrap();
    assert_eq!(t.partitions.len(), 2);
    assert!(t.warnings.contains(&TableWarning::EbrChainBroken));
}

#[test]
fn fat_boot_sector_is_not_mistaken_for_an_mbr() {
    let mut img = vec![0u8; 4096 * SS];
    let s = &mut img[..SS];
    s[0] = 0xEB;
    s[54..57].copy_from_slice(b"FAT");
    // Boot code bytes that happen to look like an entry starting at sector 1.
    put_mbr_entry(s, 0, 0x01, 1, 100);
    assert_eq!(read_partitions(&reader(img)).unwrap().kind, TableKind::None);
}

#[test]
fn garbage_status_or_empty_table_is_no_table() {
    let mut img = vec![0u8; 4096 * SS];
    put_mbr_entry(&mut img[..SS], 0, 0x0C, 2048, 100);
    img[446] = 0x12; // invalid status
    assert_eq!(
        read_partitions(&reader(img.clone())).unwrap().kind,
        TableKind::None
    );
    let mut empty = vec![0u8; 4096 * SS];
    empty[510] = 0x55;
    empty[511] = 0xAA;
    assert_eq!(read_partitions(&reader(empty)).unwrap().kind, TableKind::None);
}

#[test]
fn entries_past_the_source_are_clipped_or_dropped() {
    let mut img = vec![0u8; 4096 * SS];
    put_mbr_entry(&mut img[..SS], 0, 0x0C, 2048, 100_000);
    let t = read_partitions(&reader(img)).unwrap();
    assert_eq!(t.partitions[0].size, (4096 - 2048) * 512);
    assert!(
        t.warnings
            .contains(&TableWarning::EntryBeyondSource { number: 1 })
    );
}

#[test]
fn reads_gpt_with_name() {
    let t = read_partitions(&reader(gpt_image(8192))).unwrap();
    assert_eq!(t.kind, TableKind::Gpt);
    assert_eq!(t.partitions.len(), 1);
    let p = &t.partitions[0];
    assert_eq!((p.offset, p.size), (2048 * 512, 2048 * 512));
    assert_eq!(p.name.as_deref(), Some("Data"));
    assert!(t.warnings.is_empty());
}

#[test]
fn falls_back_to_backup_gpt_and_ignores_missing_protective_mbr() {
    let mut img = gpt_image(8192);
    img[SS..2 * SS].fill(0); // primary header gone
    img[..SS].fill(0); // protective MBR gone too
    let t = read_partitions(&reader(img)).unwrap();
    assert_eq!(t.kind, TableKind::Gpt);
    assert_eq!(t.partitions.len(), 1);
    assert_eq!(t.warnings, vec![TableWarning::PrimaryGptInvalid]);
}

#[test]
fn gpt_with_corrupt_array_crc_is_rejected() {
    let mut img = gpt_image(8192);
    img[2 * SS + 50] ^= 1;
    let last = 8191;
    img[(last - 32) * SS + 50] ^= 1;
    // Both arrays fail their CRC: no GPT; the protective MBR is reported as an MBR table.
    let t = read_partitions(&reader(img)).unwrap();
    assert_eq!(t.kind, TableKind::Mbr);
    assert_eq!(t.partitions[0].kind, PartitionType::Mbr(0xEE));
}

#[test]
fn header_with_absurd_array_is_rejected() {
    let mut img = gpt_image(8192);
    // Claim a 4 GiB entry array with a valid header CRC.
    let h = &mut img[SS..2 * SS];
    h[80..84].copy_from_slice(&u32::MAX.to_le_bytes());
    h[16..20].fill(0);
    let crc = crc32fast::hash(&h[..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    assert!(parse_gpt_header(&img[SS..2 * SS], 1, 8192).is_none());
}

#[test]
fn tiny_and_empty_sources_have_no_table() {
    assert_eq!(
        read_partitions(&reader(vec![0u8; 512])).unwrap().kind,
        TableKind::None
    );
    assert_eq!(
        read_partitions(&reader(Vec::new())).unwrap().kind,
        TableKind::None
    );
}

#[test]
fn corpus_tables_match_the_generator() {
    for s in rescyo_testkit::require_corpus!() {
        let e = &s.expected;
        let r = GuardedReader::new(s.open("none").unwrap(), ReadPolicy::default());
        let t = read_partitions(&r).unwrap();
        match e.table.as_str() {
            "none" => assert_eq!(t.kind, TableKind::None, "{}", e.scenario),
            table => {
                let want = if table == "gpt" {
                    TableKind::Gpt
                } else {
                    TableKind::Mbr
                };
                assert_eq!(t.kind, want, "{}", e.scenario);
                assert_eq!(t.partitions.len(), 1, "{}", e.scenario);
                assert_eq!(t.partitions[0].offset, e.partition.offset, "{}", e.scenario);
                assert_eq!(t.partitions[0].size, e.partition.size, "{}", e.scenario);
                assert!(t.warnings.is_empty(), "{}: {:?}", e.scenario, t.warnings);
            }
        }
        if e.corruptions.iter().any(|c| c == "mbr") && e.table == "mbr" {
            let r = GuardedReader::new(s.open("mbr").unwrap(), ReadPolicy::default());
            assert_eq!(
                read_partitions(&r).unwrap().kind,
                TableKind::None,
                "{}",
                e.scenario
            );
        }
    }
}
