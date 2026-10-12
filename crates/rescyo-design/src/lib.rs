//! Rescyo design tokens, generated from `assets/design/tokens.toml` (see `docs/design`).
//!
//! The GUI imports [`SLINT_TOKENS`] as its `Tokens` global; the TUI maps [`Palette`] colours to
//! the terminal. Components never use literals.
#![forbid(unsafe_code)]

/// An sRGB colour with alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// This colour composited over an opaque `ground` (for terminals, which have no alpha).
    pub fn over(self, ground: Rgba) -> Rgba {
        let mix = |f: u8, b: u8| {
            let a = u16::from(self.a);
            let v = (u16::from(f) * a + u16::from(b) * (255 - a) + 127) / 255;
            u8::try_from(v).unwrap_or(u8::MAX)
        };
        Rgba {
            r: mix(self.r, ground.r),
            g: mix(self.g, ground.g),
            b: mix(self.b, ground.b),
            a: 255,
        }
    }
}

/// Every colour role of one theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub ink_0: Rgba,
    pub ink_1: Rgba,
    pub ink_2: Rgba,
    pub ink_3: Rgba,
    pub line: Rgba,
    pub text: Rgba,
    pub text_2: Rgba,
    pub text_3: Rgba,
    pub rescue: Rgba,
    pub rescue_hi: Rgba,
    pub ok: Rgba,
    pub warn: Rgba,
    pub bad: Rgba,
    pub info: Rgba,
}

include!(concat!(env!("OUT_DIR"), "/tokens.rs"));

/// The `Tokens` Slint global, switching every colour on its `dark` property.
pub const SLINT_TOKENS: &str = include_str!(concat!(env!("OUT_DIR"), "/tokens.slint"));

#[cfg(test)]
mod tests {
    use super::*;

    /// docs/design/README.md must show the same colours as the tokens.
    #[test]
    fn the_design_document_matches_the_tokens() {
        let doc = include_str!("../../../docs/design/README.md");
        let hex = |c: Rgba| format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b);
        for (name, d, l) in [
            ("ink-0", DARK.ink_0, LIGHT.ink_0),
            ("ink-1", DARK.ink_1, LIGHT.ink_1),
            ("ink-2", DARK.ink_2, LIGHT.ink_2),
            ("ink-3", DARK.ink_3, LIGHT.ink_3),
            ("text", DARK.text, LIGHT.text),
            ("text-2", DARK.text_2, LIGHT.text_2),
            ("text-3", DARK.text_3, LIGHT.text_3),
            ("rescue", DARK.rescue, LIGHT.rescue),
            ("ok", DARK.ok, LIGHT.ok),
            ("warn", DARK.warn, LIGHT.warn),
            ("bad", DARK.bad, LIGHT.bad),
            ("info", DARK.info, LIGHT.info),
        ] {
            let row = format!("| `{name}` | `{}` | `{}` |", hex(d), hex(l));
            assert!(doc.contains(&row), "docs/design/README.md lacks {row}");
        }
        assert_eq!(TYPE_BODY, 13);
        assert_eq!(radius::CHIP, 6);
    }

    #[test]
    fn alpha_composites_over_the_ground() {
        let half = Rgba {
            r: 255,
            g: 255,
            b: 255,
            a: 128,
        };
        let black = Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        assert_eq!(
            half.over(black),
            Rgba {
                r: 128,
                g: 128,
                b: 128,
                a: 255
            }
        );
        assert!(SLINT_TOKENS.contains("out property <color> rescue: dark ? #ff5a1fff : #e8480fff;"));
    }
}
