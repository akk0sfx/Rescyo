// Test fixtures are tiny; casts cannot truncate.
#![allow(clippy::cast_possible_truncation)]

use super::*;

/// A 1024-byte record with fixups applied on disk, holding the given attributes.
pub fn make_record(flags: u16, number: u32, attrs: &[Vec<u8>]) -> Vec<u8> {
    let mut r = vec![0u8; 1024];
    r[0..4].copy_from_slice(b"FILE");
    r[4..6].copy_from_slice(&0x30u16.to_le_bytes());
    r[6..8].copy_from_slice(&3u16.to_le_bytes());
    r[16..18].copy_from_slice(&1u16.to_le_bytes());
    r[20..22].copy_from_slice(&0x38u16.to_le_bytes());
    r[22..24].copy_from_slice(&flags.to_le_bytes());
    r[28..32].copy_from_slice(&1024u32.to_le_bytes());
    r[44..48].copy_from_slice(&number.to_le_bytes());
    let mut o = 0x38;
    for a in attrs {
        r[o..o + a.len()].copy_from_slice(a);
        o += a.len();
    }
    r[o..o + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    r[24..28].copy_from_slice(&((o + 8) as u32).to_le_bytes());
    // Update sequence: USN 0x0042, original tail bytes saved in the array.
    let usn = 0x0042u16;
    r[0x30..0x32].copy_from_slice(&usn.to_le_bytes());
    for i in 1..3 {
        let end = i * 512 - 2;
        let (a, b) = (r[end], r[end + 1]);
        r[0x30 + i * 2] = a;
        r[0x30 + i * 2 + 1] = b;
        r[end..end + 2].copy_from_slice(&usn.to_le_bytes());
    }
    r
}

pub fn resident(kind: u32, value: &[u8]) -> Vec<u8> {
    let len = (24 + value.len()).div_ceil(8) * 8;
    let mut a = vec![0u8; len];
    a[0..4].copy_from_slice(&kind.to_le_bytes());
    a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
    a[16..20].copy_from_slice(&(value.len() as u32).to_le_bytes());
    a[20..22].copy_from_slice(&24u16.to_le_bytes());
    a[24..24 + value.len()].copy_from_slice(value);
    a
}

pub fn non_resident_data(runs: &[u8], data_size: u64) -> Vec<u8> {
    let len = (64 + runs.len()).div_ceil(8) * 8;
    let mut a = vec![0u8; len];
    a[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
    a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
    a[8] = 1;
    a[32..34].copy_from_slice(&64u16.to_le_bytes());
    a[40..48].copy_from_slice(&data_size.next_multiple_of(4096).to_le_bytes());
    a[48..56].copy_from_slice(&data_size.to_le_bytes());
    a[56..64].copy_from_slice(&data_size.to_le_bytes());
    a[64..64 + runs.len()].copy_from_slice(runs);
    a
}

pub fn file_name_value(parent: u64, namespace: u8, name: &str) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut v = vec![0u8; 66 + units.len() * 2];
    v[0..8].copy_from_slice(&parent.to_le_bytes());
    v[64] = units.len() as u8;
    v[65] = namespace;
    for (i, u) in units.iter().enumerate() {
        v[66 + i * 2..68 + i * 2].copy_from_slice(&u.to_le_bytes());
    }
    v
}

#[test]
fn fixup_restores_sector_tails_and_detects_torn_records() {
    let data = vec![7u8; 600];
    let mut r = make_record(1, 42, &[resident(ATTR_DATA, &data)]);
    let mut torn = r.clone();
    assert!(apply_fixup(&mut r, 512));
    assert_eq!(&r[510..512], &[7, 7]);
    torn[1022] ^= 0xFF;
    assert!(!apply_fixup(&mut torn, 512));
}

#[test]
fn parses_header_and_attributes() {
    let fname = file_name_value(5 | (5 << 48), 1, "Отчёт.docx");
    let mut r = make_record(
        3,
        64,
        &[
            resident(ATTR_FILE_NAME, &fname),
            non_resident_data(&[0x21, 0x10, 0x00, 0x01, 0x00], 65536),
        ],
    );
    assert!(apply_fixup(&mut r, 512));
    let h = header(&r).unwrap();
    assert!(h.in_use && h.is_dir);
    assert_eq!(h.number, Some(64));
    let attrs = attributes(&r, &h);
    assert_eq!(attrs.len(), 2);
    let AttrBody::Resident(v) = attrs[0].body else {
        panic!()
    };
    let f = file_name(v).unwrap();
    assert_eq!((f.parent & 0xFFFF_FFFF_FFFF, f.name.as_str()), (5, "Отчёт.docx"));
    let AttrBody::NonResident { runs, data_size, .. } = attrs[1].body else {
        panic!()
    };
    assert_eq!(data_size, 65536);
    assert_eq!(
        decode_runs(runs, 100, 1000).unwrap(),
        vec![Run {
            lcn: Some(256),
            clusters: 16
        }]
    );
}

#[test]
fn runlists_with_negative_offsets_and_sparse_runs() {
    // 16 clusters at LCN 0x100; 8 sparse; 4 at 0x100 - 0x10 = 0xF0.
    let runs = [0x21, 0x10, 0x00, 0x01, 0x01, 0x08, 0x11, 0x04, 0xF0, 0x00];
    assert_eq!(
        decode_runs(&runs, 100, 1000).unwrap(),
        vec![
            Run {
                lcn: Some(0x100),
                clusters: 16
            },
            Run {
                lcn: None,
                clusters: 8
            },
            Run {
                lcn: Some(0xF0),
                clusters: 4
            }
        ]
    );
}

#[test]
fn hostile_runlists_are_rejected() {
    // Offset going below zero.
    assert!(decode_runs(&[0x11, 0x01, 0x80, 0x00], 10, 100).is_none());
    // Length field of 9 bytes.
    assert!(decode_runs(&[0x19, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0], 10, 100).is_none());
    // More clusters than the attribute can hold.
    assert!(decode_runs(&[0x21, 0xFF, 0xFF, 0x01, 0x00], 10, 100).is_none());
    // Truncated run.
    assert!(decode_runs(&[0x21, 0x10], 10, 100).is_none());
    // Too many runs.
    assert!(decode_runs(&[0x11, 1, 1, 0x11, 1, 1, 0x11, 1, 1, 0], 2, 100).is_none());
}

#[test]
fn header_rejects_inconsistent_sizes() {
    let mut r = make_record(1, 1, &[]);
    assert!(apply_fixup(&mut r, 512));
    r[28..32].copy_from_slice(&4096u32.to_le_bytes());
    assert!(header(&r).is_none());
    assert!(header(b"BAAD").is_none());
}

#[test]
fn record_size_field() {
    assert_eq!(record_size(-10, 4096), Some(1024));
    assert_eq!(record_size(1, 4096), Some(4096));
    assert_eq!(record_size(-40, 4096), None);
}
