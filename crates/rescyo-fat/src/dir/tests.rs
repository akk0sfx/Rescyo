// Test fixtures are tiny; casts cannot truncate.
#![allow(clippy::cast_possible_truncation)]

use super::*;

fn ctx() -> DirContext {
    DirContext {
        fat32: true,
        max_cluster: 100_000,
        max_size: 1 << 32,
        codepage: Codepage::Cp437,
    }
}

/// 2023-07-14 18:02:10
const DATE: u16 = (43 << 9) | (7 << 5) | 14;
const TIME: u16 = (18 << 11) | (2 << 5) | 5;

fn short(name: &[u8; 11], attr: u8, cluster: u32, size: u32) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[..11].copy_from_slice(name);
    e[11] = attr;
    e[14..16].copy_from_slice(&TIME.to_le_bytes());
    e[16..18].copy_from_slice(&DATE.to_le_bytes());
    e[18..20].copy_from_slice(&DATE.to_le_bytes());
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[22..24].copy_from_slice(&TIME.to_le_bytes());
    e[24..26].copy_from_slice(&DATE.to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

/// Long-name entries for `name` in disk order (last part first), for short name `sfn`.
fn long(name: &str, sfn: &[u8; 11]) -> Vec<[u8; 32]> {
    let mut units: Vec<u16> = name.encode_utf16().collect();
    units.push(0);
    while !units.len().is_multiple_of(13) {
        units.push(0xFFFF);
    }
    let sum = short_checksum(sfn);
    let n = units.len() / 13;
    let offsets = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    (1..=n)
        .rev()
        .map(|seq| {
            let mut e = [0u8; 32];
            e[0] = seq as u8 | if seq == n { 0x40 } else { 0 };
            e[11] = 0x0F;
            e[13] = sum;
            for (i, off) in offsets.iter().enumerate() {
                e[*off..*off + 2].copy_from_slice(&units[(seq - 1) * 13 + i].to_le_bytes());
            }
            e
        })
        .collect()
}

fn cat(entries: &[[u8; 32]]) -> Vec<u8> {
    let mut d: Vec<u8> = entries.iter().flatten().copied().collect();
    d.resize(512, 0);
    d
}

#[test]
fn long_cyrillic_name_with_matching_checksum() {
    let sfn = *b"______~1DOC";
    let mut e = long("Отчёт за 2023 год.docx", &sfn);
    e.push(short(&sfn, 0x20, 4043, 49_169));
    let got = parse_dir(&cat(&e), &ctx(), 100);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name, "Отчёт за 2023 год.docx");
    assert!(got[0].has_long_name && !got[0].deleted && !got[0].is_dir);
    assert_eq!((got[0].cluster, got[0].size), (4043, 49_169));
    let m = got[0].times.modified.unwrap();
    assert_eq!(m.to_rfc3339(), "2023-07-14T18:02:10");
    assert_eq!(m.zone, TimeZone::Local);
}

#[test]
fn checksum_mismatch_falls_back_to_short_name() {
    let mut e = long("Wrong owner.txt", b"OTHER   TXT");
    e.push(short(b"README  TXT", 0x20, 5, 10));
    let got = parse_dir(&cat(&e), &ctx(), 100);
    assert_eq!(got[0].name, "README.TXT");
    assert!(!got[0].has_long_name);
}

#[test]
fn nt_case_flags_lowercase_ascii_short_names() {
    let mut s = short(b"NOTES   TXT", 0x20, 7, 300);
    s[12] = 0x18;
    assert_eq!(parse_dir(&cat(&[s]), &ctx(), 100)[0].name, "notes.txt");
}

#[test]
fn deleted_file_recovers_long_name_and_first_byte() {
    let sfn = *b"DELETE~1JPG";
    let mut e = long("deleted_photo.jpg", &sfn);
    e.push(short(&sfn, 0x20, 900, 4_196));
    for x in &mut e {
        x[0] = 0xE5;
    }
    let got = parse_dir(&cat(&e), &ctx(), 100);
    assert_eq!(got.len(), 1);
    assert!(got[0].deleted);
    assert_eq!(got[0].name, "deleted_photo.jpg");
    assert_eq!(&got[0].short, &sfn);
}

#[test]
fn deleted_short_only_entry_gets_a_placeholder_first_character() {
    let mut s = short(b"EADME   TXT", 0x20, 9, 50);
    s[0] = 0xE5;
    let got = parse_dir(&cat(&[s]), &ctx(), 100);
    assert_eq!(got[0].name, "_ADME.TXT");
    assert!(got[0].deleted && !got[0].has_long_name);
}

#[test]
fn garbage_entries_are_rejected() {
    let bad_cluster = short(b"FILE    BIN", 0x20, 200_000, 10);
    let bad_char = short(b"FI*E    BIN", 0x20, 5, 10);
    let mut bad_date = short(b"FILE2   BIN", 0x20, 5, 10);
    bad_date[24..26].copy_from_slice(&((43u16 << 9) | (13 << 5) | 1).to_le_bytes());
    let dir_with_size = short(b"DIR        ", 0x10, 5, 99);
    let huge = short(b"HUGE    BIN", 0x20, 5, u32::MAX);
    let mut small = ctx();
    small.max_size = 1 << 20;
    let got = parse_dir(
        &cat(&[bad_cluster, bad_char, bad_date, dir_with_size, huge]),
        &small,
        100,
    );
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn dot_entries_and_volume_labels_are_not_entries_but_mark_a_dir_start() {
    let dot = short(b".          ", 0x10, 4043, 0);
    let dotdot = short(b"..         ", 0x10, 1778, 0);
    let label = short(b"RESCYO     ", 0x08, 0, 0);
    let d = cat(&[dot, dotdot, label, short(b"A       TXT", 0x20, 4050, 1)]);
    assert_eq!(dir_start(&d, true), Some((4043, 1778)));
    let got = parse_dir(&d, &ctx(), 100);
    assert_eq!(
        got.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        vec!["A.TXT"]
    );
    assert!(looks_like_dir(&d, &ctx(), true));
    assert!(!looks_like_dir(&[0xAB; 512], &ctx(), false));
    assert!(!looks_like_dir(&[0u8; 512], &ctx(), true));
}

#[test]
fn oem_short_names_decode_with_the_volume_code_page() {
    // "ФОТО" in CP866, no long name (as written by DOS or old cameras)
    let s = short(
        &[0x94, 0x8E, 0x92, 0x8E, b' ', b' ', b' ', b' ', b' ', b' ', b' '],
        0x10,
        77,
        0,
    );
    let mut c = ctx();
    c.codepage = Codepage::Cp866;
    assert_eq!(parse_dir(&cat(&[s]), &c, 100)[0].name, "ФОТО");
}

#[test]
fn fat16_ignores_the_high_cluster_word() {
    let mut s = short(b"A       TXT", 0x20, 5, 10);
    s[20] = 0x12; // OS/2 extended attributes on FAT16, not a cluster
    let mut c = ctx();
    c.fat32 = false;
    assert_eq!(parse_dir(&cat(&[s]), &c, 100)[0].cluster, 5);
}

#[test]
fn entry_limit_is_enforced() {
    let e: Vec<[u8; 32]> = (0..10).map(|i| short(b"FILE    BIN", 0x20, 5 + i, 1)).collect();
    assert_eq!(parse_dir(&cat(&e), &ctx(), 3).len(), 3);
}
