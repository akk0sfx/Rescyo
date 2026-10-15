#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "tests"
)]

use super::*;

/// A catalog file record: key (parent, name) then the 248-byte file record.
fn file_record(parent: u32, name: &str, id: u32, size: u64, start: u32, blocks: u32) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let key_len = 6 + units.len() * 2;
    let mut r = Vec::new();
    r.extend_from_slice(&(key_len as u16).to_be_bytes());
    r.extend_from_slice(&parent.to_be_bytes());
    r.extend_from_slice(&(units.len() as u16).to_be_bytes());
    for u in units {
        r.extend_from_slice(&u.to_be_bytes());
    }
    if r.len() % 2 == 1 {
        r.push(0);
    }
    let mut d = vec![0u8; 248];
    d[0..2].copy_from_slice(&2u16.to_be_bytes());
    d[8..12].copy_from_slice(&id.to_be_bytes());
    d[16..20].copy_from_slice(&(3_800_000_000u32).to_be_bytes());
    d[88..96].copy_from_slice(&size.to_be_bytes());
    d[100..104].copy_from_slice(&blocks.to_be_bytes());
    d[104..108].copy_from_slice(&start.to_be_bytes());
    d[108..112].copy_from_slice(&blocks.to_be_bytes());
    r.extend(d);
    r
}

#[test]
fn file_records_parse_with_composed_names_and_dates() {
    // "й" stored decomposed, as HFS+ does.
    let r = file_record(2, "и\u{306}.txt", 40, 5000, 100, 2);
    let it = crate::walk::tests_record(&r).unwrap();
    assert_eq!(it, ("й.txt".to_owned(), 2, 40, 5000, vec![(100, 2)]));
}

#[test]
fn deleted_records_are_found_in_node_slack() {
    let mut slack = vec![0u8; 6]; // records start at even offsets
    slack.extend(file_record(16, "gone.bin", 77, 123, 9, 1));
    slack.extend([0xFF; 9]);
    let found = crate::walk::tests_slack(&slack);
    assert_eq!(found, vec![("gone.bin".to_owned(), 77)]);
}

#[test]
fn hostile_headers_are_rejected() {
    assert!(parse_header(&[0u8; 512], 0).is_none());
    let mut h = vec![0u8; 512];
    h[0..2].copy_from_slice(b"H+");
    h[2..4].copy_from_slice(&4u16.to_be_bytes());
    h[40..44].copy_from_slice(&3u32.to_be_bytes()); // block size not a power of two
    assert!(parse_header(&h, 0).is_none());
}
