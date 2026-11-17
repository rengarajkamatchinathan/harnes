//! Themes (07 §1.2): semantic role tokens; a light/dark guess from the terminal palette
//! (luminance); truecolor → 256 → ANSI 16 fallback; `NO_COLOR` respected.

use ratatui::style::{Color, Modifier, Style};

/// What a span means; the theme decides how it looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Text,
    Muted,
    Accent,
    User,
    Success,
    Warning,
    Error,
    Code,
    Heading,
    Link,
    DiffAdd,
    DiffDel,
    DiffHunk,
    Thinking,
    Selected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorDepth {
    /// `NO_COLOR` or a dumb terminal: modifiers only.
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub depth: ColorDepth,
    pub dark: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            depth: ColorDepth::Ansi16,
            dark: true,
        }
    }
}

impl Theme {
    /// From the environment: `NO_COLOR`, `COLORTERM`, `TERM`, `COLORFGBG`.
    pub fn detect(env: impl Fn(&str) -> Option<String>) -> Theme {
        let depth = if env("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            ColorDepth::None
        } else {
            let colorterm = env("COLORTERM").unwrap_or_default().to_lowercase();
            let term = env("TERM").unwrap_or_default().to_lowercase();
            if colorterm == "truecolor" || colorterm == "24bit" {
                ColorDepth::TrueColor
            } else if term == "dumb" {
                ColorDepth::None
            } else if term.contains("256color") {
                ColorDepth::Ansi256
            } else {
                ColorDepth::Ansi16
            }
        };
        // `COLORFGBG="15;0"`: the last field is the background's ANSI index.
        let dark = env("COLORFGBG")
            .and_then(|v| v.rsplit(';').next().and_then(|b| b.parse::<u8>().ok()))
            .map(ansi_index_is_dark)
            .unwrap_or(true);
        Theme { depth, dark }
    }

    /// With a queried background colour (OSC 11), guess light/dark by luminance.
    pub fn with_background(mut self, rgb: (u8, u8, u8)) -> Theme {
        self.dark = is_dark(rgb);
        self
    }

    fn rgb(&self, role: Role) -> Option<(u8, u8, u8)> {
        let d = self.dark;
        Some(match role {
            Role::Text => return None,
            Role::Muted | Role::Thinking => {
                if d {
                    (0x8a, 0x8f, 0x98)
                } else {
                    (0x6a, 0x6f, 0x78)
                }
            }
            Role::Accent | Role::Selected => {
                if d {
                    (0xc7, 0x9b, 0xff)
                } else {
                    (0x7c, 0x3a, 0xed)
                }
            }
            Role::User => {
                if d {
                    (0x7d, 0xcf, 0xff)
                } else {
                    (0x02, 0x69, 0xa1)
                }
            }
            Role::Success | Role::DiffAdd => {
                if d {
                    (0x73, 0xd2, 0x8b)
                } else {
                    (0x15, 0x80, 0x3d)
                }
            }
            Role::Warning => {
                if d {
                    (0xff, 0xc8, 0x57)
                } else {
                    (0xa1, 0x62, 0x07)
                }
            }
            Role::Error | Role::DiffDel => {
                if d {
                    (0xff, 0x7b, 0x72)
                } else {
                    (0xb9, 0x1c, 0x1c)
                }
            }
            Role::Code => {
                if d {
                    (0xe6, 0xc0, 0x7b)
                } else {
                    (0x92, 0x40, 0x0e)
                }
            }
            Role::Heading => {
                if d {
                    (0xff, 0xff, 0xff)
                } else {
                    (0x11, 0x18, 0x27)
                }
            }
            Role::Link | Role::DiffHunk => {
                if d {
                    (0x61, 0xaf, 0xef)
                } else {
                    (0x1d, 0x4e, 0xd8)
                }
            }
        })
    }

    pub fn color(&self, rgb: (u8, u8, u8)) -> Option<Color> {
        let (r, g, b) = rgb;
        match self.depth {
            ColorDepth::None => None,
            ColorDepth::TrueColor => Some(Color::Rgb(r, g, b)),
            ColorDepth::Ansi256 => Some(Color::Indexed(rgb_to_256(r, g, b))),
            ColorDepth::Ansi16 => Some(rgb_to_16(r, g, b)),
        }
    }

    pub fn style(&self, role: Role) -> Style {
        let mut s = Style::default();
        if let Some(c) = self.rgb(role).and_then(|rgb| self.color(rgb)) {
            s = s.fg(c);
        }
        match role {
            Role::Heading => s.add_modifier(Modifier::BOLD),
            Role::Muted => s.add_modifier(if self.depth == ColorDepth::None {
                Modifier::DIM
            } else {
                Modifier::empty()
            }),
            Role::Thinking => s.add_modifier(Modifier::ITALIC | Modifier::DIM),
            Role::Link => s.add_modifier(Modifier::UNDERLINED),
            Role::Selected => s.add_modifier(Modifier::BOLD | Modifier::REVERSED),
            Role::User | Role::Accent => s.add_modifier(Modifier::BOLD),
            _ => s,
        }
    }
}

fn ansi_index_is_dark(bg: u8) -> bool {
    // 0–6 and 8 are dark backgrounds in the standard palette; 7 and 9–15 are light.
    matches!(bg, 0..=6 | 8)
}

/// Relative luminance (sRGB) below 0.5 → dark.
pub fn is_dark((r, g, b): (u8, u8, u8)) -> bool {
    fn lin(c: u8) -> f64 {
        let c = f64::from(c) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    let l = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
    l < 0.18 // perceptual mid-grey (L* 50) is ~0.18 relative luminance
}

/// Nearest xterm-256 index (6×6×6 cube or the grey ramp).
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    fn cube(v: u8) -> u8 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            (v - 35) / 40
        }
    }
    let (cr, cg, cb) = (cube(r), cube(g), cube(b));
    let level = |c: u8| if c == 0 { 0 } else { 55 + 40 * u32::from(c) };
    let cube_rgb = (level(cr), level(cg), level(cb));
    let avg = (u32::from(r) + u32::from(g) + u32::from(b)) / 3;
    let grey_idx = if avg > 238 {
        23
    } else {
        avg.saturating_sub(3) / 10
    };
    let grey = 8 + 10 * grey_idx;
    let dist = |(x, y, z): (u32, u32, u32)| {
        let d = |a: u32, b: u8| (a as i64 - i64::from(b)).pow(2);
        d(x, r) + d(y, g) + d(z, b)
    };
    if dist((grey, grey, grey)) < dist(cube_rgb) {
        232 + grey_idx as u8
    } else {
        16 + 36 * cr + 6 * cg + cb
    }
}

/// Nearest of the 16 ANSI colours (by hue family and brightness).
pub fn rgb_to_16(r: u8, g: u8, b: u8) -> Color {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let bright = max > 0xb0;
    if max - min < 0x20 {
        return match max {
            0..=0x40 => Color::Black,
            0x41..=0x90 => Color::DarkGray,
            0x91..=0xd0 => Color::Gray,
            _ => Color::White,
        };
    }
    let (hr, hg, hb) = (r > max / 2 + 0x10, g > max / 2 + 0x10, b > max / 2 + 0x10);
    match (hr, hg, hb, bright) {
        (true, false, false, false) => Color::Red,
        (true, false, false, true) => Color::LightRed,
        (false, true, false, false) => Color::Green,
        (false, true, false, true) => Color::LightGreen,
        (true, true, false, false) => Color::Yellow,
        (true, true, false, true) => Color::LightYellow,
        (false, false, true, false) => Color::Blue,
        (false, false, true, true) => Color::LightBlue,
        (true, false, true, false) => Color::Magenta,
        (true, false, true, true) => Color::LightMagenta,
        (false, true, true, false) => Color::Cyan,
        (false, true, true, true) => Color::LightCyan,
        _ => Color::White,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn depth_detection_and_no_color() {
        assert_eq!(
            Theme::detect(env(&[("COLORTERM", "truecolor")])).depth,
            ColorDepth::TrueColor
        );
        assert_eq!(
            Theme::detect(env(&[("TERM", "xterm-256color")])).depth,
            ColorDepth::Ansi256
        );
        assert_eq!(
            Theme::detect(env(&[("TERM", "xterm")])).depth,
            ColorDepth::Ansi16
        );
        let t = Theme::detect(env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")]));
        assert_eq!(t.depth, ColorDepth::None);
        assert_eq!(t.style(Role::Error).fg, None);
    }

    #[test]
    fn light_dark_guess() {
        assert!(!Theme::detect(env(&[("COLORFGBG", "0;15")])).dark);
        assert!(Theme::detect(env(&[("COLORFGBG", "15;0")])).dark);
        assert!(is_dark((0x1e, 0x1e, 0x1e)));
        assert!(!is_dark((0xfa, 0xfa, 0xfa)));
    }

    #[test]
    fn fallbacks() {
        assert_eq!(rgb_to_256(255, 0, 0), 196);
        assert_eq!(rgb_to_256(0, 0, 0), 16);
        assert_eq!(rgb_to_256(128, 128, 128), 244);
        assert_eq!(rgb_to_16(0xff, 0x7b, 0x72), Color::LightRed);
        assert_eq!(rgb_to_16(0x15, 0x80, 0x3d), Color::Green);
        let t = Theme {
            depth: ColorDepth::Ansi256,
            dark: true,
        };
        assert!(matches!(t.style(Role::Success).fg, Some(Color::Indexed(_))));
    }
}
