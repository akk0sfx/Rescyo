// Fixtures are small; casts cannot truncate.
#![allow(clippy::cast_possible_truncation)]

use std::io::Cursor;

use super::*;

fn check(data: &[u8], name: &str) -> Verdict {
    verify(&mut Cursor::new(data.to_vec()), data.len() as u64, name)
}

fn png() -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(data);
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc.finalize().to_be_bytes());
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let (w, h) = (64u32, 64u32);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            raw.extend_from_slice(&[(x * 4) as u8, (y * 4) as u8, 128]);
        }
    }
    chunk(
        &mut out,
        b"IDAT",
        &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6),
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

fn jpeg() -> Vec<u8> {
    let (w, h) = (96u16, 64u16);
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[(x * 2) as u8, (y * 3) as u8, ((x + y) % 256) as u8]);
        }
    }
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 85)
        .encode(&px, w, h, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    out
}

fn gif() -> Vec<u8> {
    b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;"
        .to_vec()
}

fn zip() -> Vec<u8> {
    let members: [(&str, Vec<u8>, bool); 2] = [
        ("[Content_Types].xml", b"<Types/>".repeat(40), false),
        (
            "word/document.xml",
            b"<w:document>Hello, recovered world.</w:document>".repeat(200),
            true,
        ),
    ];
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for (name, data, deflate) in &members {
        let comp = if *deflate {
            miniz_oxide::deflate::compress_to_vec(data, 6)
        } else {
            data.clone()
        };
        let crc = crc32fast::hash(data);
        let local = out.len() as u32;
        let method: u16 = if *deflate { 8 } else { 0 };
        out.extend_from_slice(b"PK\x03\x04\x14\x00\x00\x00");
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&comp);
        cd.extend_from_slice(b"PK\x01\x02\x14\x00\x14\x00\x00\x00");
        cd.extend_from_slice(&method.to_le_bytes());
        cd.extend_from_slice(&[0, 0, 0, 0]);
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
        cd.extend_from_slice(&[0; 12]);
        cd.extend_from_slice(&local.to_le_bytes());
        cd.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(b"PK\x05\x06\x00\x00\x00\x00");
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

fn pdf() -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R >>",
    ];
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

fn mp4() -> Vec<u8> {
    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }
    let mut out = bx(b"ftyp", b"isom\0\0\x02\0isommp41");
    out.extend(bx(
        b"moov",
        &[bx(b"mvhd", &[0u8; 100]), bx(b"trak", &bx(b"tkhd", &[0u8; 84]))].concat(),
    ));
    out.extend(bx(b"mdat", &[7u8; 2000]));
    out
}

fn samples() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a.png", png()),
        ("b.jpg", jpeg()),
        ("c.gif", gif()),
        ("d.docx", zip()),
        ("e.pdf", pdf()),
        ("f.mp4", mp4()),
    ]
}

#[test]
fn intact_files_verify() {
    for (name, data) in samples() {
        assert_eq!(check(&data, name), Verdict::Intact, "{name}");
    }
}

#[test]
fn truncation_is_detected() {
    for (name, data) in samples() {
        let cut = &data[..data.len() * 2 / 3];
        assert!(matches!(check(cut, name), Verdict::Damaged(_)), "{name}");
    }
}

#[test]
fn a_zeroed_sector_or_flipped_bytes_in_the_middle_are_detected() {
    for (name, data) in samples() {
        if name == "c.gif" || name == "f.mp4" || name == "e.pdf" {
            continue; // structural checks only: payload bytes are not covered
        }
        let mut z = data.clone();
        let mid = z.len() / 2;
        let end = (mid + 512).min(z.len() - 2);
        z[mid..end].fill(0);
        assert!(matches!(check(&z, name), Verdict::Damaged(_)), "{name} zeroed");
        // A wrong cluster in the middle: what a broken chain or a contiguous guess produces.
        let mut g = data.clone();
        let mut x: u32 = 0x1234_5678;
        for b in &mut g[mid..end] {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
        assert!(
            matches!(check(&g, name), Verdict::Damaged(_)),
            "{name} foreign cluster"
        );
        // Checksummed formats also catch single flipped bytes; JPEG has no checksum and may not.
        if name != "b.jpg" {
            let mut f = data.clone();
            f[mid] ^= 0x5A;
            assert!(matches!(check(&f, name), Verdict::Damaged(_)), "{name} flipped");
        }
    }
}

#[test]
fn wrong_content_for_the_name_is_damage_and_unknown_formats_are_unverified() {
    assert!(matches!(check(&png(), "photo.jpg"), Verdict::Damaged(_)));
    assert!(matches!(check(&[0x5Au8; 4096], "photo.jpg"), Verdict::Damaged(_)));
    assert_eq!(check(b"plain text", "notes.txt"), Verdict::Unverified);
    assert_eq!(check(&png(), "mystery.bin"), Verdict::Intact);
}

#[test]
fn zip_bomb_ratios_are_not_inflated() {
    let mut z = zip();
    // Claim 2 GiB uncompressed for the second member in the central directory.
    let cd = z.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    let second = cd + 1 + z[cd + 1..].windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    z[second + 24..second + 28].copy_from_slice(&0x8000_0000u32.to_le_bytes());
    assert_eq!(check(&z, "bomb.zip"), Verdict::Unverified);
}

#[test]
#[ignore = "helper: prints a small JPEG for the corpus generator"]
fn print_small_jpeg() {
    let (w, h) = (16u16, 16u16);
    let px: Vec<u8> = (0..u32::from(w) * u32::from(h))
        .flat_map(|i| [(i * 7) as u8, (i * 3) as u8, 200])
        .collect();
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 80)
        .encode(&px, w, h, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    let hex: String = out.iter().map(|b| format!("{b:02x}")).collect();
    println!("JPEG_HEX={hex}");
}
