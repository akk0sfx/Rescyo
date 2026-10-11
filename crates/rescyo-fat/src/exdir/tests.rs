// Test fixtures are tiny; casts cannot truncate.
#![allow(clippy::cast_possible_truncation)]

use super::*;

fn ctx() -> ExContext {
    ExContext {
        max_cluster: 10_000,
        cluster_size: 4096,
        max_size: 1 << 40,
    }
}

/// Builds a live entry set for `name`.
fn set(name: &str, dir: bool, first: u32, len: u64, no_fat_chain: bool) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let names = units.len().div_ceil(15);
    let mut s = vec![0u8; 32 * (2 + names)];
    s[0] = 0x85;
    s[1] = (1 + names) as u8;
    s[4..6].copy_from_slice(&(if dir { 0x10u16 } else { 0x20 }).to_le_bytes());
    // 2020-09-13 12:26:40 local, with UTC offset +3:00 (12 steps of 15 minutes).
    let raw: u32 = (40 << 25) | (9 << 21) | (13 << 16) | (12 << 11) | (26 << 5) | 20;
    s[12..16].copy_from_slice(&raw.to_le_bytes());
    s[23] = 0x80 | 12;
    s[32] = 0xC0;
    s[33] = 1 | if no_fat_chain { 2 } else { 0 };
    s[35] = units.len() as u8;
    s[40..48].copy_from_slice(&len.to_le_bytes());
    s[52..56].copy_from_slice(&first.to_le_bytes());
    s[56..64].copy_from_slice(&len.to_le_bytes());
    for (i, u) in units.iter().enumerate() {
        let off = 64 + (i / 15) * 32 + 2 + (i % 15) * 2;
        s[64 + (i / 15) * 32] = 0xC1;
        s[off..off + 2].copy_from_slice(&u.to_le_bytes());
    }
    let sum = set_checksum(&s, false);
    s[2..4].copy_from_slice(&sum.to_le_bytes());
    s
}

#[test]
fn parses_a_live_set_with_a_long_unicode_name() {
    let name = "東京タワー and a name longer than fifteen characters.jpg";
    let s = set(name, false, 100, 5000, true);
    let (e, _) = parse_exdir(&s, &ctx(), 10);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].name, name);
    assert!(e[0].no_fat_chain && !e[0].deleted && !e[0].is_dir);
    assert_eq!((e[0].first_cluster, e[0].data_len), (100, 5000));
    let m = e[0].times.modified.unwrap();
    assert_eq!(m.zone, TimeZone::Utc);
    assert_eq!(m.to_rfc3339(), "2020-09-13T09:26:40Z");
}

#[test]
fn deleted_set_verifies_with_in_use_restored() {
    let mut s = set("удалённый документ.txt", false, 200, 900, true);
    for o in (0..s.len()).step_by(32) {
        s[o] &= 0x7F;
    }
    let (e, _) = parse_exdir(&s, &ctx(), 10);
    assert_eq!(e.len(), 1);
    assert!(e[0].deleted);
    assert_eq!(e[0].name, "удалённый документ.txt");
}

#[test]
fn corrupted_set_is_rejected() {
    let mut s = set("photo.jpg", false, 100, 10, false);
    s[70] ^= 1;
    assert!(parse_exdir(&s, &ctx(), 10).0.is_empty());
    let mut s = set("dir", true, 100, 1000, false); // not a multiple of the cluster
    let sum = set_checksum(&s, false);
    s[2..4].copy_from_slice(&sum.to_le_bytes());
    assert!(parse_exdir(&s, &ctx(), 10).0.is_empty());
    let s = set("far.bin", false, 50_000, 10, false); // beyond the last cluster
    assert!(parse_exdir(&s, &ctx(), 10).0.is_empty());
}

#[test]
fn root_system_entries_are_reported() {
    let mut d = vec![0u8; 4096];
    d[0] = 0x83;
    d[1] = 6;
    for (i, u) in "RESCYO".encode_utf16().enumerate() {
        d[2 + i * 2..4 + i * 2].copy_from_slice(&u.to_le_bytes());
    }
    d[32] = 0x81;
    d[52..56].copy_from_slice(&2u32.to_le_bytes());
    d[56..64].copy_from_slice(&1024u64.to_le_bytes());
    d[64] = 0x82;
    d[84..88].copy_from_slice(&3u32.to_le_bytes());
    d[88..96].copy_from_slice(&5836u64.to_le_bytes());
    let s = set("a.txt", false, 9, 1, true);
    d[96..96 + s.len()].copy_from_slice(&s);
    let (e, info) = parse_exdir(&d, &ctx(), 10);
    assert_eq!(e.len(), 1);
    assert_eq!(info.label.as_deref(), Some("RESCYO"));
    assert_eq!(info.bitmap, Some((2, 1024)));
    assert_eq!(info.upcase, Some((3, 5836, 0)));
    assert!(looks_like_exdir(&d, &ctx(), true));
    assert!(!looks_like_exdir(&[0x5Au8; 4096], &ctx(), false));
}
