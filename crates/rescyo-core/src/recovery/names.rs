//! Turning recovered names into names every Target can hold.

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// Which naming rules apply to a Target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameRules {
    /// Windows and FAT/exFAT/NTFS rules: no `<>:"\|?*`, no trailing dot or space, no device names.
    pub windows: bool,
    /// macOS: `:` is shown as `/` by Finder, so it is replaced too.
    pub mac: bool,
    /// Names differing only in case (and Unicode normalization) collide.
    pub case_insensitive: bool,
}

impl NameRules {
    /// The strictest rules for this OS combined with the Target file system's.
    pub fn for_target(target_windows_fs: bool, case_insensitive: bool) -> Self {
        Self {
            windows: cfg!(windows) || target_windows_fs,
            mac: cfg!(target_os = "macos"),
            case_insensitive,
        }
    }
}

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Most bytes (UTF-8, for Unix) or UTF-16 units (Windows, macOS) in one name component.
const MAX_NAME: usize = 255;

/// Makes one path component legal. Returns the new name (equal to `name` if nothing changed).
pub fn sanitize(name: &str, rules: NameRules) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '/' | '\0' => '_',
            c if rules.windows && (c < ' ' || "<>:\"\\|?*".contains(c)) => '_',
            ':' if rules.mac => '_',
            c => c,
        })
        .collect();
    if rules.windows {
        while s.ends_with(['.', ' ']) {
            s.pop();
        }
        let stem = s.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
        if RESERVED.contains(&stem.as_str()) {
            s.insert(0, '_');
        }
    }
    if s.is_empty() || s == "." || s == ".." {
        s = format!("_{s}");
    }
    if s.len() > MAX_NAME || s.encode_utf16().count() > MAX_NAME {
        s = shorten(&s);
    }
    s
}

/// Cuts a long name, keeping its extension and adding a short hash so different long names
/// stay different.
fn shorten(s: &str) -> String {
    let hash: String = Sha256::digest(s.as_bytes())
        .iter()
        .take(3)
        .map(|b| format!("{b:02x}"))
        .collect();
    let (stem, ext) = split_ext(s);
    let ext = if ext.len() <= 16 { ext } else { "" };
    let budget = 200usize.saturating_sub(ext.len());
    let mut cut = String::new();
    for c in stem.chars() {
        if cut.len() + c.len_utf8() > budget {
            break;
        }
        cut.push(c);
    }
    format!("{cut}~{hash}{ext}")
}

/// Splits `name.ext` into (`name`, `.ext`); dotfiles and names without a dot have no extension.
pub fn split_ext(s: &str) -> (&str, &str) {
    match s.rfind('.') {
        Some(i) if i > 0 => (s.get(..i).unwrap_or(s), s.get(i..).unwrap_or("")),
        _ => (s, ""),
    }
}

/// Tracks names already used in each Target directory and resolves collisions.
#[derive(Debug, Default)]
pub struct Namer {
    used: HashMap<String, HashSet<String>>,
}

impl Namer {
    fn key(name: &str, rules: NameRules) -> String {
        let n: String = name.nfc().collect();
        if rules.case_insensitive {
            n.to_lowercase()
        } else {
            n
        }
    }

    /// Reserves a name in directory `dir`, appending ` (2)`, ` (3)`… if it is taken.
    pub fn reserve(&mut self, dir: &str, name: &str, rules: NameRules) -> String {
        let used = self.used.entry(dir.to_owned()).or_default();
        if used.insert(Self::key(name, rules)) {
            return name.to_owned();
        }
        let (stem, ext) = split_ext(name);
        for i in 2u32.. {
            let cand = format!("{stem} ({i}){ext}");
            if used.insert(Self::key(&cand, rules)) {
                return cand;
            }
        }
        unreachable!("u32 suffixes exhausted")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: NameRules = NameRules {
        windows: true,
        mac: false,
        case_insensitive: true,
    };
    const UNIX: NameRules = NameRules {
        windows: false,
        mac: false,
        case_insensitive: false,
    };

    #[test]
    fn windows_rules() {
        assert_eq!(sanitize("a<b>c:d\"e\\f|g?h*i.txt", WIN), "a_b_c_d_e_f_g_h_i.txt");
        assert_eq!(sanitize("trailing. . ", WIN), "trailing");
        assert_eq!(sanitize("CON", WIN), "_CON");
        assert_eq!(sanitize("nul.txt", WIN), "_nul.txt");
        assert_eq!(sanitize("console.txt", WIN), "console.txt");
        assert_eq!(sanitize("tab\there", WIN), "tab_here");
        assert_eq!(sanitize("...", WIN), "_");
    }

    #[test]
    fn unix_keeps_everything_but_slash_and_nul() {
        assert_eq!(sanitize("a:b?c*.txt", UNIX), "a:b?c*.txt");
        assert_eq!(sanitize("a/b\0c", UNIX), "a_b_c");
        assert_eq!(sanitize("..", UNIX), "_..");
        assert_eq!(sanitize("", UNIX), "_");
    }

    #[test]
    fn long_names_are_shortened_distinctly_and_keep_the_extension() {
        let a = format!("{}A.jpeg", "x".repeat(300));
        let b = format!("{}B.jpeg", "x".repeat(300));
        let sa = sanitize(&a, UNIX);
        assert!(sa.len() <= 255 && sa.ends_with(".jpeg"));
        assert_ne!(sa, sanitize(&b, UNIX));
        let cjk = "東".repeat(200);
        assert!(sanitize(&cjk, UNIX).len() <= 255);
    }

    #[test]
    fn collisions_by_case_and_normalization() {
        let mut n = Namer::default();
        assert_eq!(n.reserve("d", "Photo.JPG", WIN), "Photo.JPG");
        assert_eq!(n.reserve("d", "photo.jpg", WIN), "photo (2).jpg");
        assert_eq!(n.reserve("d", "photo.jpg", WIN), "photo (3).jpg");
        assert_eq!(n.reserve("other", "photo.jpg", WIN), "photo.jpg");
        // "й" precomposed and decomposed are the same name on macOS and Windows.
        assert_eq!(n.reserve("d", "\u{0439}.txt", WIN), "\u{0439}.txt");
        assert_eq!(
            n.reserve("d", "\u{0438}\u{0306}.txt", WIN),
            "\u{0438}\u{0306} (2).txt"
        );
        let mut u = Namer::default();
        assert_eq!(u.reserve("d", "A", UNIX), "A");
        assert_eq!(u.reserve("d", "a", UNIX), "a");
    }
}
