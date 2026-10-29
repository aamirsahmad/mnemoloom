//! Shared TUI palette ("Inspector": gruvbox-dark based) used by the search
//! TUI, the session menu and the mux picker.

use ratatui::style::{Color, Modifier, Style};

pub const BG: Color = Color::Rgb(0x1d, 0x20, 0x21);
/// Background of the selected row/card.
pub const BG_SEL: Color = Color::Rgb(0x32, 0x30, 0x2f);
pub const BORDER: Color = Color::Rgb(0x50, 0x49, 0x45);
pub const FG: Color = Color::Rgb(0xeb, 0xdb, 0xb2);
/// Body text one step quieter than `FG` (snippets).
pub const FG_SOFT: Color = Color::Rgb(0xd5, 0xc4, 0xa1);
pub const DIM: Color = Color::Rgb(0xa8, 0x99, 0x84);
/// Focus, section headings, the current match.
pub const ACCENT: Color = Color::Rgb(0xfa, 0xbd, 0x2f);
/// Search-term highlight in lists.
pub const MATCH: Color = Color::Rgb(0xfe, 0x80, 0x19);
pub const CLAUDE: Color = Color::Rgb(0xfe, 0x80, 0x19);
pub const CODEX: Color = Color::Rgb(0x83, 0xa5, 0x98);
pub const PI: Color = Color::Rgb(0xd3, 0x86, 0x9b);
pub const OPENCODE: Color = Color::Rgb(0xb8, 0xbb, 0x26);
/// User messages, checked boxes, live markers.
pub const OK: Color = Color::Rgb(0x8e, 0xc0, 0x7c);
/// Lineage and links.
pub const LINK: Color = Color::Rgb(0x83, 0xa5, 0x98);
pub const WARN: Color = Color::Rgb(0xfb, 0x49, 0x34);

pub fn text() -> Style {
    Style::new().fg(FG)
}

pub fn dim() -> Style {
    Style::new().fg(DIM)
}

pub fn heading() -> Style {
    Style::new().fg(ACCENT)
}

/// Bold label such as a key in the footer.
pub fn key() -> Style {
    Style::new().fg(FG).add_modifier(Modifier::BOLD)
}

pub fn border(focused: bool) -> Style {
    Style::new().fg(if focused { ACCENT } else { BORDER })
}

/// Inverse chip, e.g. the footer mode label.
pub fn chip() -> Style {
    Style::new().fg(BG).bg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Highlight for the current search match.
pub fn hit_current() -> Style {
    Style::new().fg(BG).bg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Highlight for other search matches.
pub fn hit() -> Style {
    Style::new().fg(BG).bg(MATCH).add_modifier(Modifier::BOLD)
}
