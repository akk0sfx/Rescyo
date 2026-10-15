//! Design tokens mapped to the terminal: 24-bit colour where the terminal has it, the nearest
//! xterm-256 colour otherwise, and no colour at all under `NO_COLOR` (every state also carries a
//! glyph and a word, so nothing is lost).

use ratatui::style::{Color, Modifier, Style};
use rescyo_design::{Palette, Rgba};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    TrueColor,
    Indexed,
    None,
}

impl Depth {
    pub fn detect() -> Self {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Self::None;
        }
        match std::env::var("COLORTERM").as_deref() {
            Ok("truecolor" | "24bit") => Self::TrueColor,
            // Windows Terminal and modern consoles render 24-bit colour.
            _ if std::env::var_os("WT_SESSION").is_some() => Self::TrueColor,
            _ => Self::Indexed,
        }
    }
}

/// The colours of one theme, resolved for the terminal.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub depth: Depth,
    pub p: Palette,
}

impl Theme {
    pub fn new(dark: bool, depth: Depth) -> Self {
        Self {
            depth,
            p: if dark {
                rescyo_design::DARK
            } else {
                rescyo_design::LIGHT
            },
        }
    }

    /// A token colour; translucent tokens are composited over `ink-1`.
    pub fn c(&self, c: Rgba) -> Color {
        let c = if c.a < 255 { c.over(self.p.ink_1) } else { c };
        match self.depth {
            Depth::TrueColor => Color::Rgb(c.r, c.g, c.b),
            Depth::Indexed => Color::Indexed(xterm256(c)),
            Depth::None => Color::Reset,
        }
    }

    pub fn fg(&self, c: Rgba) -> Style {
        if self.depth == Depth::None {
            return Style::default();
        }
        Style::default().fg(self.c(c))
    }

    /// Text on a ground.
    pub fn on(&self, fg: Rgba, bg: Rgba) -> Style {
        if self.depth == Depth::None {
            return Style::default();
        }
        Style::default().fg(self.c(fg)).bg(self.c(bg))
    }

    /// The window ground for an area.
    pub fn ground(&self, bg: Rgba) -> Style {
        self.on(self.p.text, bg)
    }

    /// The single primary action: white on rescue orange (reversed without colour).
    pub fn primary(&self) -> Style {
        if self.depth == Depth::None {
            return Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
        }
        Style::default()
            .fg(self.c(Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            }))
            .bg(self.c(self.p.rescue))
            .add_modifier(Modifier::BOLD)
    }

    /// Selection: `ink-3` ground, or reversed without colour.
    pub fn selected(&self) -> Style {
        if self.depth == Depth::None {
            return Style::default().add_modifier(Modifier::REVERSED);
        }
        Style::default().bg(self.c(self.p.ink_3)).fg(self.c(self.p.text))
    }
}

/// The nearest colour of the xterm 256-colour palette (6×6×6 cube and 24 greys).
pub fn xterm256(c: Rgba) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let near = |v: u8| -> (usize, u8) {
        let mut best = (0usize, u8::MAX);
        for (i, &l) in LEVELS.iter().enumerate() {
            let d = v.abs_diff(l);
            if d < best.1 {
                best = (i, d);
            }
        }
        (best.0, LEVELS.get(best.0).copied().unwrap_or(0))
    };
    let dist = |a: (u8, u8, u8), b: (u8, u8, u8)| -> u32 {
        let d = |x: u8, y: u8| u32::from(x.abs_diff(y)).pow(2);
        d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
    };
    let (ri, rv) = near(c.r);
    let (gi, gv) = near(c.g);
    let (bi, bv) = near(c.b);
    let cube = u8::try_from(16 + 36 * ri + 6 * gi + bi).unwrap_or(16);
    let cube_d = dist((c.r, c.g, c.b), (rv, gv, bv));
    let avg = (u16::from(c.r) + u16::from(c.g) + u16::from(c.b)) / 3;
    let gi = u8::try_from((avg.saturating_sub(3) / 10).min(23)).unwrap_or(23);
    let gray = 8 + 10 * gi;
    let gray_d = dist((c.r, c.g, c.b), (gray, gray, gray));
    if gray_d < cube_d { 232 + gi } else { cube }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_maps_to_xterm() {
        assert_eq!(
            xterm256(Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255
            }),
            16
        );
        assert_eq!(
            xterm256(Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255
            }),
            231
        );
        // Rescue orange stays orange (cube 202 = #ff5f00).
        assert_eq!(xterm256(rescyo_design::DARK.rescue), 202);
        // The near-black window ground becomes a dark grey, not a colour.
        assert!(xterm256(rescyo_design::DARK.ink_0) >= 232);
    }
}
