use std::sync::Arc;

use rescyo_fs::VecSink;
use rescyo_io::ReadPolicy;
use rescyo_io::testing::MemSource;

use super::*;

fn jpeg(scan: usize) -> Vec<u8> {
    let mut j = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    j.extend_from_slice(b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0");
    j.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x04, 0x00, 0x01]);
    j.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x04, 0x01, 0x00]);
    for i in 0..scan {
        j.push((i * 7 % 251) as u8);
        if i % 97 == 0 {
            j.extend_from_slice(&[0xFF, 0x00]); // stuffed byte
        }
        if i % 1000 == 999 {
            j.extend_from_slice(&[0xFF, 0xD3]); // restart marker
        }
    }
    j.retain(|_| true);
    j.extend_from_slice(&[0xFF, 0xD9]);
    j
}

fn png(complete: bool) -> Vec<u8> {
    let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
    let chunk = |p: &mut Vec<u8>, kind: &[u8], data: &[u8]| {
        p.extend_from_slice(&(data.len() as u32).to_be_bytes());
        p.extend_from_slice(kind);
        p.extend_from_slice(data);
        p.extend_from_slice(&[0, 0, 0, 0]);
    };
    chunk(&mut p, b"IHDR", &[0; 13]);
    chunk(&mut p, b"IDAT", &[0x55; 3000]);
    if complete {
        chunk(&mut p, b"IEND", &[]);
    }
    p
}

/// A stored ZIP with one member, laid out like a Word package.
fn docx(member: &[u8]) -> Vec<u8> {
    let name = b"[Content_Types].xml";
    let name2 = b"word/document.xml";
    let mut z = Vec::new();
    let mut central = Vec::new();
    for (n, data) in [(&name[..], &b"<Types/>"[..]), (&name2[..], member)] {
        let off = z.len() as u32;
        z.extend_from_slice(b"PK\x03\x04\x14\0\0\0\0\0\0\0\0\0\0\0\0\0");
        z.extend_from_slice(&(data.len() as u32).to_le_bytes());
        z.extend_from_slice(&(data.len() as u32).to_le_bytes());
        z.extend_from_slice(&(n.len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(n);
        z.extend_from_slice(data);
        central.extend_from_slice(b"PK\x01\x02\x14\0\x14\0\0\0\0\0\0\0\0\0\0\0\0\0");
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(n.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(n);
    }
    let cd_at = z.len() as u32;
    z.extend_from_slice(&central);
    z.extend_from_slice(b"PK\x05\x06\0\0\0\0\x02\0\x02\0");
    z.extend_from_slice(&(central.len() as u32).to_le_bytes());
    z.extend_from_slice(&cd_at.to_le_bytes());
    z.extend_from_slice(&3u16.to_le_bytes());
    z.extend_from_slice(b"hi!");
    z
}

fn mp4() -> Vec<u8> {
    let atom = |kind: &[u8], body: usize| {
        let mut a = ((body + 8) as u32).to_be_bytes().to_vec();
        a.extend_from_slice(kind);
        a.extend(std::iter::repeat_n(0x11, body));
        a
    };
    let mut m = atom(b"ftyp", 16);
    m[8..12].copy_from_slice(b"qt  ");
    m.extend(atom(b"moov", 500));
    m.extend(atom(b"mdat", 9000));
    m
}

fn pdf() -> Vec<u8> {
    let mut p = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\nxref\n0 1\ntrailer\n<<>>\n%%EOF\n".to_vec();
    // An incremental update.
    p.extend_from_slice(b"2 0 obj\n<<>>\nendobj\nxref\n0 1\ntrailer\n<<>>\n%%EOF\n");
    p
}

fn gif() -> Vec<u8> {
    let mut g = b"GIF89a\x02\0\x02\0\x80\0\0".to_vec();
    g.extend_from_slice(&[0; 6]); // two-colour global table
    g.extend_from_slice(&[0x21, 0xF9, 4, 0, 0, 0, 0, 0]);
    g.extend_from_slice(&[0x2C, 0, 0, 0, 0, 2, 0, 2, 0, 0, 2, 2, 0x44, 0x01, 0]);
    g.push(0x3B);
    g
}

struct Disk {
    img: Vec<u8>,
    placed: Vec<(u64, Vec<u8>)>,
}

impl Disk {
    fn new(size: usize) -> Self {
        Self {
            img: vec![0u8; size],
            placed: Vec::new(),
        }
    }
    fn put(&mut self, sector: usize, data: Vec<u8>) {
        let at = sector * 512;
        self.img[at..at + data.len()].copy_from_slice(&data);
        self.placed.push((at as u64, data));
    }
    fn carve(&self, known: &HashSet<u64>) -> Vec<Carved> {
        let r = GuardedReader::new(
            Arc::new(MemSource::new(self.img.clone(), 512)),
            ReadPolicy::default(),
        );
        let mut c = Carver::new();
        for (i, chunk) in self.img.chunks(1 << 20).enumerate() {
            c.feed((i << 20) as u64, chunk);
        }
        c.finish(&r, known, &CancelToken::new()).unwrap()
    }
}

#[test]
fn every_format_is_carved_to_its_exact_end() {
    let mut d = Disk::new(4 << 20);
    d.put(10, jpeg(5000));
    d.put(100, png(true));
    d.put(200, docx(b"<w:document/>"));
    d.put(300, mp4());
    d.put(400, pdf());
    d.put(500, gif());
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&1000u32.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend(vec![0u8; 996]);
    d.put(600, wav);
    let got = d.carve(&HashSet::new());
    let exts: Vec<&str> = got.iter().map(|c| c.ext).collect();
    assert_eq!(exts, ["jpg", "png", "docx", "mov", "pdf", "gif", "wav"]);
    for (c, (at, data)) in got.iter().zip(&d.placed) {
        assert_eq!((c.offset, c.len), (*at, data.len() as u64), "{}", c.ext);
    }
}

#[test]
fn broken_files_are_dropped_and_embedded_or_known_ones_skipped() {
    let mut d = Disk::new(1 << 20);
    d.put(10, png(false)); // no IEND, then zeros: not a file
    // A JPEG stored inside a ZIP member, landing on a sector start: part of the ZIP.
    let mut member = vec![0u8; 512 - 30 - 17 - 30 - 19 - 8 + 512];
    member.extend(jpeg(100));
    let z = docx(&member);
    d.put(100, z);
    d.put(300, gif());
    let got = d.carve(&[300 * 512].into_iter().collect());
    let exts: Vec<&str> = got.iter().map(|c| c.ext).collect();
    assert_eq!(exts, ["docx"]);
}

#[test]
fn emit_groups_by_extension() {
    let files = vec![
        Carved {
            offset: 0x1000,
            len: 10,
            format: Format::Jpeg,
            ext: "jpg",
        },
        Carved {
            offset: 0x3000,
            len: 20,
            format: Format::Pdf,
            ext: "pdf",
        },
        Carved {
            offset: 0x5000,
            len: 30,
            format: Format::Jpeg,
            ext: "jpg",
        },
    ];
    let mut sink = VecSink::default();
    let stats = emit(&files, &mut sink).unwrap();
    assert_eq!(stats.get("jpg"), Some(&2));
    let paths: Vec<String> = (1..=sink.entries.len() as u64)
        .map(|i| sink.path_of(rescyo_fs::EntryId(i)))
        .collect();
    assert_eq!(
        paths,
        [
            "jpg",
            "jpg/000000001000.jpg",
            "pdf",
            "pdf/000000003000.pdf",
            "jpg/000000005000.jpg"
        ]
    );
}

#[test]
fn a_size_field_past_the_end_of_the_disk_is_not_a_file() {
    let mut d = Disk::new(64 << 10);
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&(1u32 << 30).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    d.put(4, wav);
    assert!(d.carve(&HashSet::new()).is_empty());
}
