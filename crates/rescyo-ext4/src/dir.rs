//! Directory blocks: live entries, and deleted ones still readable in the slack space a
//! deletion leaves behind (the previous entry simply grows over a removed one).

use rescyo_io::bytes::{slice_at, u8_at, u16_at, u32_at};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub inode: u32,
    pub name: String,
    /// 1 regular file, 2 directory (0 when the file system does not record types).
    pub file_type: u8,
    pub deleted: bool,
}

fn real_len(name_len: usize) -> usize {
    (8 + name_len).div_ceil(4) * 4
}

/// Parses one directory block. `slack` enables the search for deleted entries (off for hashed
/// directory index blocks, whose slack holds index data, not names).
pub fn parse_block(b: &[u8], slack: bool, max_inode: u32, out: &mut Vec<DirEntry>) {
    let mut o = 0usize;
    // An index node of a hashed directory: one empty entry spanning the block.
    if u32_at(b, 0) == Some(0) && u16_at(b, 4).map(usize::from) == Some(b.len()) && u8_at(b, 6) == Some(0) {
        return;
    }
    while o + 8 <= b.len() {
        let (Some(ino), Some(rec), Some(nl), Some(ft)) =
            (u32_at(b, o), u16_at(b, o + 4), u8_at(b, o + 6), u8_at(b, o + 7))
        else {
            return;
        };
        let rec = usize::from(rec);
        let nl = usize::from(nl);
        if rec < 8 || rec % 4 != 0 || o + rec > b.len() || nl + 8 > rec {
            return;
        }
        // Checksum tail (inode 0, name length 0, type 0xDE): not an entry.
        let name = slice_at(b, o + 8, nl).and_then(|n| std::str::from_utf8(n).ok());
        if ino != 0 && nl > 0 && ino <= max_inode {
            if let Some(n) = name {
                out.push(DirEntry {
                    inode: ino,
                    name: n.to_owned(),
                    file_type: ft,
                    deleted: false,
                });
            }
        } else if ino == 0 && nl > 0 && slack && ft != 0xDE {
            // The first entry of a block is deleted by zeroing its inode: the name stays.
            if let Some(n) = name {
                out.push(DirEntry {
                    inode: 0,
                    name: n.to_owned(),
                    file_type: ft,
                    deleted: true,
                });
            }
        }
        if slack {
            deleted_in(b, o + real_len(nl), o + rec, max_inode, out);
        }
        o += rec;
    }
}

/// Deleted entries inside `b[from..to]`.
fn deleted_in(b: &[u8], mut p: usize, to: usize, max_inode: u32, out: &mut Vec<DirEntry>) {
    while p + 8 <= to {
        let ok = (|| {
            let ino = u32_at(b, p)?;
            let rec = usize::from(u16_at(b, p + 4)?);
            let nl = usize::from(u8_at(b, p + 6)?);
            let ft = u8_at(b, p + 7)?;
            if ino == 0 || ino > max_inode || nl == 0 || ft > 7 || rec < real_len(nl) || p + 8 + nl > to {
                return None;
            }
            let name = std::str::from_utf8(slice_at(b, p + 8, nl)?).ok()?;
            if name.contains(['/', '\0']) || name == "." || name == ".." {
                return None;
            }
            Some((
                DirEntry {
                    inode: ino,
                    name: name.to_owned(),
                    file_type: ft,
                    deleted: true,
                },
                real_len(nl),
            ))
        })();
        match ok {
            Some((e, len)) => {
                out.push(e);
                p += len;
            }
            None => p += 4,
        }
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, reason = "test fixtures")]
mod tests {
    use super::*;

    fn entry(b: &mut Vec<u8>, ino: u32, rec: u16, name: &str, ft: u8) {
        b.extend_from_slice(&ino.to_le_bytes());
        b.extend_from_slice(&rec.to_le_bytes());
        b.push(name.len() as u8);
        b.push(ft);
        b.extend_from_slice(name.as_bytes());
        while !b.len().is_multiple_of(4) {
            b.push(0);
        }
    }

    #[test]
    fn live_and_deleted_entries() {
        let mut b = Vec::new();
        entry(&mut b, 2, 12, ".", 2);
        entry(&mut b, 2, 12, "..", 2);
        // "photo.jpg" grew over the deleted "old.txt" that followed it.
        entry(&mut b, 12, 20 + 16, "photo.jpg", 1);
        entry(&mut b, 13, 16, "old.txt", 1);
        let rest = 1024 - b.len();
        entry(&mut b, 14, rest as u16, "last", 1);
        b.resize(1024, 0);
        let mut out = Vec::new();
        parse_block(&b, true, 1000, &mut out);
        let names: Vec<(&str, bool)> = out.iter().map(|e| (e.name.as_str(), e.deleted)).collect();
        assert_eq!(
            names,
            [
                (".", false),
                ("..", false),
                ("photo.jpg", false),
                ("old.txt", true),
                ("last", false)
            ]
        );
        // Without slack search the deleted name stays hidden.
        out.clear();
        parse_block(&b, false, 1000, &mut out);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn hostile_blocks_stop_cleanly() {
        let mut out = Vec::new();
        parse_block(&[0xFF; 64], true, 10, &mut out);
        parse_block(&[0; 3], true, 10, &mut out);
        let mut b = Vec::new();
        entry(&mut b, 5, 4, "x", 1); // record shorter than its header
        b.resize(64, 0);
        parse_block(&b, true, 10, &mut out);
        assert!(out.is_empty());
    }
}
