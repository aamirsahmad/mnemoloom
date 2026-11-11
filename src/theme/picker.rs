//! Live colour-theme picker modal (`ctrl-o`) shared by the search TUI, the
//! session menu and the mux picker.
//!
//! Moving the cursor or typing a filter immediately applies the highlighted
//! theme via [`super::apply`], so the UI behind the popup recolours on the
//! next frame. Pressing `Enter` saves the choice to the [config file](crate::config);
//! `Esc` (or `ctrl-o` again) restores the theme that was active when the
//! picker opened.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::builtin::fold;
use super::{Theme, apply, catalog, current_name, palette};
use crate::config::Config;
use crate::index::tui::text::{self, fit};

const PAGE: usize = 10;
const CHROME_ROWS: u16 = 4;
const SWATCH_W: usize = 9;

/// State of an open theme picker modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    pub themes: Vec<Theme>,
    pub query: String,
    pub cursor: usize,
    /// Theme active when the picker opened; restored on `Esc`.
    pub original: Theme,
}

/// What happened after [`Picker::on_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Picker remains open (live preview updated).
    Stay,
    /// Closed with `Esc` or `^o`; original theme restored.
    Cancelled,
    /// Confirmed with `Enter`; theme applied and saved to the config file.
    Selected {
        name: String,
        saved: Result<PathBuf, String>,
    },
    /// `^c`: original theme restored; caller should quit.
    Quit,
}

impl Picker {
    /// Open the picker using the user's config file (or the built-ins in tests).
    pub fn open() -> Self {
        #[cfg(not(test))]
        let cfg = crate::config::load().unwrap_or_default();
        #[cfg(test)]
        let cfg = Config::default();
        Self::from_config(&cfg)
    }

    /// Open the picker from an explicit [`Config`].
    pub fn from_config(cfg: &Config) -> Self {
        let (themes, _) = catalog(cfg);
        let cur = current_name();
        let cursor = themes
            .iter()
            .position(|t| fold(&t.name) == fold(&cur))
            .unwrap_or(0);
        let original = themes.get(cursor).cloned().unwrap_or_else(|| Theme {
            name: cur,
            palette: palette(),
            custom: false,
            light: false,
        });
        Picker {
            themes,
            query: String::new(),
            cursor,
            original,
        }
    }

    /// Themes matching [`Self::query`].
    pub fn filtered(&self) -> Vec<&Theme> {
        let words: Vec<&str> = self.query.split_whitespace().collect();
        if words.is_empty() {
            return self.themes.iter().collect();
        }
        self.themes
            .iter()
            .filter(|t| words.iter().all(|w| matches_word(t, w)))
            .collect()
    }

    /// Currently highlighted theme, if any match.
    pub fn selected(&self) -> Option<&Theme> {
        let items = self.filtered();
        items
            .get(self.cursor.min(items.len().saturating_sub(1)))
            .copied()
    }

    /// Restore the theme that was active when the picker opened.
    pub fn cancel(&self) {
        apply(&self.original);
    }

    fn preview_current(&self) {
        if let Some(t) = self.selected() {
            apply(t);
        }
    }

    /// Handle one key press.
    pub fn on_key(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let len = self.filtered().len();
        let last = len.saturating_sub(1);
        self.cursor = self.cursor.min(last);

        match k.code {
            KeyCode::Char('c') if ctrl => {
                self.cancel();
                return Outcome::Quit;
            }
            KeyCode::Esc => {
                self.cancel();
                return Outcome::Cancelled;
            }
            KeyCode::Char('o') if ctrl => {
                self.cancel();
                return Outcome::Cancelled;
            }
            KeyCode::Enter => {
                let Some(chosen) = self.selected().cloned() else {
                    return Outcome::Stay;
                };
                apply(&chosen);
                let saved = save_selected(&chosen.name);
                return Outcome::Selected {
                    name: chosen.name,
                    saved,
                };
            }
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('p' | 'k') if ctrl => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Char('n' | 'j') if ctrl => self.cursor = (self.cursor + 1).min(last),
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(PAGE),
            KeyCode::PageDown => self.cursor = (self.cursor + PAGE).min(last),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = last,
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                let cur = current_name();
                self.cursor = self
                    .themes
                    .iter()
                    .position(|t| fold(&t.name) == fold(&cur))
                    .unwrap_or(0);
            }
            KeyCode::Backspace | KeyCode::Delete => {
                self.query.pop();
                self.cursor = 0;
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.query.push(c);
                self.cursor = 0;
            }
            _ => return Outcome::Stay,
        }
        self.preview_current();
        Outcome::Stay
    }
}

fn matches_word(t: &Theme, word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    if lower == "light" && t.light {
        return true;
    }
    if lower == "dark" && !t.light {
        return true;
    }
    if lower == "custom" && t.custom {
        return true;
    }
    if t.name.to_ascii_lowercase().contains(&lower) {
        return true;
    }
    let folded = fold(word).replace("catpuccin", "catppuccin");
    !folded.is_empty() && fold(&t.name).contains(&folded)
}

#[cfg(not(test))]
fn save_selected(name: &str) -> Result<PathBuf, String> {
    crate::config::save_theme(name).map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
fn save_selected(_name: &str) -> Result<PathBuf, String> {
    Ok(PathBuf::from("config.toml"))
}

/// Draw the centred theme picker modal over `area`.
pub fn draw(frame: &mut Frame, picker: &Picker, area: Rect) {
    let items = picker.filtered();
    let h = (items.len() as u16 + CHROME_ROWS)
        .clamp(8, 26)
        .min(area.height);
    let w = 56u16.min(area.width);
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let p = palette();
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(super::border(true))
        .title(Span::styled(
            " THEME ",
            super::heading().add_modifier(Modifier::BOLD),
        ))
        .style(super::text().bg(p.bg));
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    frame.render_widget(block, rect);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let width = usize::from(inner.width);
    let prompt = format!("/ {}▏", picker.query);
    let count = match items.len() {
        1 => "1 theme".to_string(),
        n => format!("{n} themes"),
    };
    let room = width.saturating_sub(text::width(&prompt) + 1);
    let top = Line::from(vec![
        Span::styled("/ ", super::heading().add_modifier(Modifier::BOLD)),
        Span::styled(picker.query.clone(), super::text()),
        Span::styled("▏", super::heading()),
        Span::raw(" "),
        Span::styled(fit(&count, room, true), super::dim()),
    ]);
    frame.render_widget(Paragraph::new(top), Rect { height: 1, ..inner });

    if inner.height > 1 {
        let rule = Line::styled("─".repeat(width), super::border(false));
        frame.render_widget(
            Paragraph::new(rule),
            Rect {
                y: inner.y + 1,
                height: 1,
                ..inner
            },
        );
    }
    if inner.height <= 2 {
        return;
    }
    let list = Rect {
        y: inner.y + 2,
        height: inner.height - 2,
        ..inner
    };
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("No matching themes", super::dim())),
            list,
        );
        return;
    }

    let rows = usize::from(list.height);
    let cursor = picker.cursor.min(items.len().saturating_sub(1));
    let skip = (cursor + 1).saturating_sub(rows);
    let show_swatches = width >= 32;
    let swatch_room = if show_swatches { SWATCH_W + 1 } else { 0 };

    for (i, t) in items.iter().enumerate().skip(skip).take(rows) {
        let selected = i == cursor;
        let line_area = Rect {
            y: list.y + (i - skip) as u16,
            height: 1,
            ..list
        };
        if selected {
            let r = line_area.intersection(frame.area());
            frame.buffer_mut().set_style(r, Style::new().bg(p.bg_sel));
        }
        let bar = if selected {
            Span::styled("▌", super::heading())
        } else {
            Span::raw(" ")
        };
        let active = fold(&t.name) == fold(&picker.original.name);
        let mark = if active {
            Span::styled("● ", Style::new().fg(p.ok))
        } else {
            Span::raw("  ")
        };
        let tag = match (t.custom, t.light) {
            (true, true) => " (custom, light)",
            (true, false) => " (custom)",
            (false, true) => " (light)",
            (false, false) => "",
        };
        let label_room = width.saturating_sub(4 + swatch_room);
        let name_room = label_room.saturating_sub(tag.len());
        let name_st = if selected {
            super::text().add_modifier(Modifier::BOLD)
        } else {
            super::text()
        };
        let name_shown = text::truncate(&t.name, name_room);
        let used = text::width(&name_shown) + tag.len();
        let pad = label_room.saturating_sub(used);

        let mut spans = vec![bar, Span::raw(" "), mark, Span::styled(name_shown, name_st)];
        if !tag.is_empty() {
            spans.push(Span::styled(tag, super::dim()));
        }
        spans.push(Span::raw(" ".repeat(pad)));
        if show_swatches {
            spans.push(Span::raw(" "));
            spans.extend(swatches(&t.palette));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), line_area);
    }
}

fn swatches(p: &super::Palette) -> Vec<Span<'static>> {
    let cols = [p.accent, p.ok, p.link, p.pi, p.warn];
    let mut out = Vec::with_capacity(cols.len() * 2);
    for (i, c) in cols.into_iter().enumerate() {
        if i > 0 {
            out.push(Span::raw(" "));
        }
        out.push(Span::styled("●", Style::new().fg(c)));
    }
    out
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::config::CustomTheme;
    use crate::theme::builtin::{DEFAULT, GRUVBOX_DARK};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn reset() {
        let (themes, _) = catalog(&Config::default());
        apply(&themes[0]);
    }

    #[test]
    fn live_preview_and_cancel_restores_original() {
        reset();
        let mut p = Picker::open();
        assert_eq!(p.original.name, DEFAULT);
        assert_eq!(p.cursor, 0);
        // Moving down previews gruvbox-light, then nord.
        assert_eq!(p.on_key(key(KeyCode::Down)), Outcome::Stay);
        assert_eq!(current_name(), "gruvbox-light");
        assert_eq!(p.on_key(ctrl('n')), Outcome::Stay);
        assert_eq!(current_name(), "nord");
        assert_ne!(palette(), GRUVBOX_DARK);
        // Esc restores gruvbox-dark.
        assert_eq!(p.on_key(key(KeyCode::Esc)), Outcome::Cancelled);
        assert_eq!(current_name(), DEFAULT);
        assert_eq!(palette(), GRUVBOX_DARK);
    }

    #[test]
    fn filter_by_name_alias_light_and_custom() {
        reset();
        let cfg = Config {
            theme: None,
            themes: vec![CustomTheme {
                name: "my-custom".into(),
                base: Some("nord".into()),
                colors: Vec::new(),
            }],
        };
        let mut p = Picker::from_config(&cfg);
        for c in "catpuccin".chars() {
            assert_eq!(p.on_key(key(KeyCode::Char(c))), Outcome::Stay);
        }
        assert_eq!(p.filtered().len(), 4);
        assert_eq!(current_name(), "catppuccin-mocha");

        // ^u clears the query and keeps the cursor on the active theme.
        p.on_key(ctrl('u'));
        assert!(p.query.is_empty());
        assert_eq!(p.selected().unwrap().name, "catppuccin-mocha");

        // "light" finds all 7 light themes, including latte, day and dawn.
        p.on_key(ctrl('u'));
        for c in "light".chars() {
            p.on_key(key(KeyCode::Char(c)));
        }
        let lights: Vec<&str> = p.filtered().iter().map(|t| t.name.as_str()).collect();
        assert!(lights.contains(&"catppuccin-latte"));
        assert!(lights.contains(&"tokyo-night-day"));
        assert!(lights.contains(&"rose-pine-dawn"));
        assert!(p.filtered().iter().all(|t| t.light));

        // "custom" finds custom themes.
        p.on_key(ctrl('u'));
        for c in "custom".chars() {
            p.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(p.filtered().len(), 1);
        assert_eq!(p.selected().unwrap().name, "my-custom");

        // Enter selects and keeps the theme active.
        assert!(matches!(
            p.on_key(key(KeyCode::Enter)),
            Outcome::Selected { ref name, .. } if name == "my-custom"
        ));
        assert_eq!(current_name(), "my-custom");
        reset();
    }

    #[test]
    fn ctrl_o_and_ctrl_c_restore_original() {
        reset();
        let mut p = Picker::open();
        p.on_key(key(KeyCode::End));
        assert_eq!(current_name(), "terminal");
        assert_eq!(p.on_key(ctrl('o')), Outcome::Cancelled);
        assert_eq!(current_name(), DEFAULT);

        let mut p = Picker::open();
        p.on_key(key(KeyCode::Down));
        assert_eq!(p.on_key(ctrl('c')), Outcome::Quit);
        assert_eq!(current_name(), DEFAULT);
    }

    #[test]
    fn draw_renders_and_handles_tiny_areas() {
        reset();
        let mut p = Picker::open();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &p, f.area())).unwrap();
        let buf = term.backend().buffer();
        let screen: String = (0..30)
            .map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains(" THEME "), "{screen}");
        assert!(screen.contains("25 themes"), "{screen}");
        assert!(screen.contains("● gruvbox-dark"), "{screen}");
        assert!(screen.contains("gruvbox-light (light)"), "{screen}");
        assert!(screen.contains("nord"), "{screen}");

        // Empty filter match does not panic and shows "No matching themes".
        for c in "zzzz".chars() {
            p.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(p.on_key(key(KeyCode::Enter)), Outcome::Stay);
        term.draw(|f| draw(f, &p, f.area())).unwrap();
        let buf = term.backend().buffer();
        let screen: String = (0..30)
            .map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("No matching themes"), "{screen}");

        for (w, h) in [(1, 1), (5, 2), (15, 4), (28, 8)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| draw(f, &p, f.area())).unwrap();
        }
        reset();
    }
}
