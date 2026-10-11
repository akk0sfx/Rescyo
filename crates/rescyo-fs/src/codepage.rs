//! OEM code pages for short (8.3) names and other legacy single-byte or DBCS names.
//!
//! FAT short names are stored in the OEM code page of the machine that wrote them. Which one is
//! not recorded, so it is guessed from the names themselves ([`detect`]) or chosen by the user.

use encoding_rs::{BIG5, EUC_KR, Encoding, GBK, IBM866, SHIFT_JIS};

/// Supported OEM code pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codepage {
    /// United States (the FAT default).
    Cp437,
    /// Western Europe.
    Cp850,
    /// Russian.
    Cp866,
    /// Japanese (Shift-JIS).
    Cp932,
    /// Simplified Chinese (GBK).
    Cp936,
    /// Korean (Unified Hangul Code, a superset of EUC-KR).
    Cp949,
    /// Traditional Chinese (Big5).
    Cp950,
}

impl Codepage {
    pub const ALL: [Codepage; 7] = [
        Self::Cp437,
        Self::Cp850,
        Self::Cp866,
        Self::Cp932,
        Self::Cp936,
        Self::Cp949,
        Self::Cp950,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Cp437 => "cp437",
            Self::Cp850 => "cp850",
            Self::Cp866 => "cp866",
            Self::Cp932 => "cp932",
            Self::Cp936 => "cp936",
            Self::Cp949 => "cp949",
            Self::Cp950 => "cp950",
        }
    }

    /// Accepts `cp866`, `866`, `ibm866`, `shift_jis`, `gbk`, `euc-kr`, `big5` and similar.
    pub fn from_name(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        let s = s.trim_start_matches("cp").trim_start_matches("ibm");
        Some(match s {
            "437" => Self::Cp437,
            "850" => Self::Cp850,
            "866" => Self::Cp866,
            "932" | "shift_jis" | "shift-jis" | "sjis" => Self::Cp932,
            "936" | "gbk" | "gb2312" => Self::Cp936,
            "949" | "euc-kr" | "uhc" => Self::Cp949,
            "950" | "big5" => Self::Cp950,
            _ => return None,
        })
    }

    /// Decodes bytes; undecodable sequences become U+FFFD.
    pub fn decode(self, bytes: &[u8]) -> String {
        match self {
            Self::Cp437 => single_byte(bytes, &CP437_HIGH),
            Self::Cp850 => single_byte(bytes, &CP850_HIGH),
            Self::Cp866 => with(IBM866, bytes).0,
            Self::Cp932 => with(SHIFT_JIS, bytes).0,
            Self::Cp936 => with(GBK, bytes).0,
            Self::Cp949 => with(EUC_KR, bytes).0,
            Self::Cp950 => with(BIG5, bytes).0,
        }
    }

    fn decode_strict(self, bytes: &[u8]) -> Option<String> {
        match self {
            Self::Cp437 | Self::Cp850 => Some(self.decode(bytes)),
            Self::Cp866 => strict(IBM866, bytes),
            Self::Cp932 => strict(SHIFT_JIS, bytes),
            Self::Cp936 => strict(GBK, bytes),
            Self::Cp949 => strict(EUC_KR, bytes),
            Self::Cp950 => strict(BIG5, bytes),
        }
    }
}

fn with(enc: &'static Encoding, bytes: &[u8]) -> (String, bool) {
    let (s, had_errors) = enc.decode_without_bom_handling(bytes);
    (s.into_owned(), had_errors)
}

fn strict(enc: &'static Encoding, bytes: &[u8]) -> Option<String> {
    let (s, bad) = with(enc, bytes);
    (!bad).then_some(s)
}

fn single_byte(bytes: &[u8], high: &[char; 128]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b < 0x80 {
                char::from(b)
            } else {
                high.get(usize::from(b - 0x80)).copied().unwrap_or('\u{FFFD}')
            }
        })
        .collect()
}

/// Guesses the code page from raw names. `None` when no name has a byte above 0x7F (then any
/// code page decodes identically).
///
/// Some encodings share byte ranges completely (EUC-KR Hangul and GB2312 hanzi), so a
/// near-tie is broken in favour of `prefer` (normally the code page of the user's locale).
pub fn detect<'a>(names: impl IntoIterator<Item = &'a [u8]>, prefer: Option<Codepage>) -> Option<Codepage> {
    let samples: Vec<&[u8]> = names
        .into_iter()
        .filter(|n| n.iter().any(|&b| b >= 0x80))
        .take(4096)
        .collect();
    if samples.is_empty() {
        return None;
    }
    let scored: Vec<(i64, Codepage)> = Codepage::ALL
        .iter()
        .map(|&cp| {
            let score = samples
                .iter()
                .map(|s| match cp.decode_strict(s) {
                    Some(text) => name_score(&text),
                    None => -50,
                })
                .sum();
            (score, cp)
        })
        .collect();
    // First maximum wins, so earlier code pages are preferred on exact ties.
    let best = scored
        .iter()
        .copied()
        .fold(None, |acc: Option<(i64, Codepage)>, cur| match acc {
            Some(b) if b.0 >= cur.0 => Some(b),
            _ => Some(cur),
        })?;
    if let Some(p) = prefer
        && let Some(&(score, _)) = scored.iter().find(|(_, cp)| *cp == p)
        && best.0 > 0
        && score * 10 >= best.0 * 9
    {
        return Some(p);
    }
    Some(best.1)
}

/// How plausible a decoded short name is: script-appropriate letters score, symbols and
/// script mixing inside a word cost.
fn name_score(text: &str) -> i64 {
    let mut score = 0;
    let mut prev: Option<char> = None;
    for c in text.chars() {
        if !c.is_ascii() {
            score += char_score(c);
        }
        if let Some(p) = prev
            && (is_cyrillic(p) && c.is_ascii_alphabetic() || p.is_ascii_alphabetic() && is_cyrillic(c))
        {
            score -= 3;
        }
        prev = Some(c);
    }
    score
}

fn is_cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&c)
}

/// How plausible a non-ASCII character is in a short name.
fn char_score(c: char) -> i64 {
    match c {
        '\u{FFFD}' => -20,
        // Short names are stored upper-case: lower-case Cyrillic hints at a wrong guess.
        c if is_cyrillic(c) && c.is_uppercase() => 3,
        c if is_cyrillic(c) => -1,
        // CJK ideographs, kana, Hangul syllables: one character spans two bytes.
        c if ('\u{4E00}'..='\u{9FFF}').contains(&c)
            || ('\u{3040}'..='\u{30FF}').contains(&c)
            || ('\u{AC00}'..='\u{D7A3}').contains(&c) =>
        {
            6
        }
        // Box drawing, block elements, math symbols: typical of a wrong single-byte guess
        c if ('\u{2500}'..='\u{259F}').contains(&c) || ('\u{2200}'..='\u{22FF}').contains(&c) => -4,
        // Half-width katakana: legal but rare in names, typical of a wrong Shift-JIS guess
        c if ('\u{FF61}'..='\u{FF9F}').contains(&c) => -2,
        c if c.is_alphabetic() => 1,
        _ => -1,
    }
}

const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', //
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', //
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', //
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', //
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', //
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀', //
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', //
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{A0}',
];

const CP850_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', //
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', 'ø', '£', 'Ø', '×', 'ƒ', //
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '®', '¬', '½', '¼', '¡', '«', '»', //
    '░', '▒', '▓', '│', '┤', 'Á', 'Â', 'À', '©', '╣', '║', '╗', '╝', '¢', '¥', '┐', //
    '└', '┴', '┬', '├', '─', '┼', 'ã', 'Ã', '╚', '╔', '╩', '╦', '╠', '═', '╬', '¤', //
    'ð', 'Ð', 'Ê', 'Ë', 'È', 'ı', 'Í', 'Î', 'Ï', '┘', '┌', '█', '▄', '¦', 'Ì', '▀', //
    'Ó', 'ß', 'Ô', 'Ò', 'õ', 'Õ', 'µ', 'þ', 'Þ', 'Ú', 'Û', 'Ù', 'ý', 'Ý', '¯', '´', //
    '\u{AD}', '±', '‗', '¾', '¶', '§', '÷', '¸', '°', '¨', '·', '¹', '³', '²', '■', '\u{A0}',
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_known_names() {
        // "ОТЧЕТ" in CP866
        assert_eq!(Codepage::Cp866.decode(&[0x8E, 0x92, 0x97, 0x85, 0x92]), "ОТЧЕТ");
        assert_eq!(Codepage::Cp437.decode(&[0x80, b'A']), "ÇA");
        assert_eq!(Codepage::Cp850.decode(&[0x9B]), "ø");
        // "東京" in Shift-JIS
        assert_eq!(Codepage::Cp932.decode(&[0x93, 0x8C, 0x8B, 0x9E]), "東京");
    }

    #[test]
    fn detects_russian_japanese_chinese_korean() {
        let ru: Vec<Vec<u8>> = vec![
            IBM866.encode("ОТЧЕТ").0.into_owned(),
            IBM866.encode("ФОТО").0.into_owned(),
            IBM866.encode("ДОКУМЕНТ").0.into_owned(),
        ];
        assert_eq!(detect(ru.iter().map(Vec::as_slice), None), Some(Codepage::Cp866));
        let ja: Vec<Vec<u8>> = ["写真", "テスト", "東京"]
            .iter()
            .map(|s| SHIFT_JIS.encode(s).0.into_owned())
            .collect();
        assert_eq!(detect(ja.iter().map(Vec::as_slice), None), Some(Codepage::Cp932));
        let ko: Vec<Vec<u8>> = vec![
            EUC_KR.encode("사진").0.into_owned(),
            EUC_KR.encode("문서").0.into_owned(),
        ];
        // Hangul and hanzi share bytes: the locale decides a tie.
        assert_eq!(
            detect(ko.iter().map(Vec::as_slice), Some(Codepage::Cp949)),
            Some(Codepage::Cp949)
        );
        let zh: Vec<Vec<u8>> = vec![
            GBK.encode("照片").0.into_owned(),
            GBK.encode("文档").0.into_owned(),
        ];
        assert_eq!(detect(zh.iter().map(Vec::as_slice), None), Some(Codepage::Cp936));
        // A locale preference does not override a clear winner.
        assert_eq!(
            detect(ru.iter().map(Vec::as_slice), Some(Codepage::Cp932)),
            Some(Codepage::Cp866)
        );
    }

    #[test]
    fn ascii_only_needs_no_guess_and_names_parse() {
        assert_eq!(detect([b"README  TXT".as_slice()], None), None);
        assert_eq!(Codepage::from_name("CP866"), Some(Codepage::Cp866));
        assert_eq!(Codepage::from_name("shift_jis"), Some(Codepage::Cp932));
        assert_eq!(Codepage::from_name("koi8"), None);
    }
}
