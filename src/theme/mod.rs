//! TUI colour themes shared by the search TUI, the session menu and the mux
//! picker.
//!
//! The active [`Palette`] is process-wide. [`init`] picks it at startup
//! (`$MNEMOLOOM_THEME`, else `theme` in the [config file](crate::config),
//! else [`builtin::DEFAULT`]); the ctrl-o [`picker`] changes it live.
//! Renderers read it through [`palette`] and the style helpers below on
//! every draw, so a change shows on the next frame.

pub mod builtin;
pub mod picker;

use std::borrow::Cow;
use std::str::FromStr;

use ratatui::style::{Color, Modifier, Style};

use crate::config::{self, Config};
use builtin::{BUILTINS, DEFAULT, GRUVBOX_DARK, fold};

/// Env override for the theme name (wins over the config file).
pub const THEME_ENV: &str = "MNEMOLOOM_THEME";

/// The colour roles every theme fills in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub bg: Color,
    /// Background of the selected row/card.
    pub bg_sel: Color,
    pub border: Color,
    pub fg: Color,
    /// Body text one step quieter than `fg` (snippets).
    pub fg_soft: Color,
    pub dim: Color,
    /// Focus, section headings, the current match.
    pub accent: Color,
    /// Search-term highlight in lists.
    pub matched: Color,
    pub claude: Color,
    pub codex: Color,
    pub pi: Color,
    pub opencode: Color,
    pub agy: Color,
    /// User messages, checked boxes, live markers.
    pub ok: Color,
    /// Lineage and links.
    pub link: Color,
    pub warn: Color,
}

/// Role names as written in `[themes.<name>]` config tables.
pub const ROLES: &[&str] = &[
    "bg", "bg_sel", "border", "fg", "fg_soft", "dim", "accent", "match", "claude", "codex", "pi",
    "opencode", "agy", "ok", "link", "warn",
];

impl Palette {
    /// The colour for a role name (see [`ROLES`]; `-` and `_` are
    /// interchangeable, and `selection` / `matched` are accepted too).
    fn role_mut(&mut self, role: &str) -> Option<&mut Color> {
        let role = role.trim().to_ascii_lowercase().replace('-', "_");
        Some(match role.as_str() {
            "bg" | "background" => &mut self.bg,
            "bg_sel" | "selection" => &mut self.bg_sel,
            "border" => &mut self.border,
            "fg" | "foreground" | "text" => &mut self.fg,
            "fg_soft" => &mut self.fg_soft,
            "dim" => &mut self.dim,
            "accent" => &mut self.accent,
            "match" | "matched" => &mut self.matched,
            "claude" => &mut self.claude,
            "codex" => &mut self.codex,
            "pi" => &mut self.pi,
            "opencode" => &mut self.opencode,
            "agy" => &mut self.agy,
            "ok" => &mut self.ok,
            "link" => &mut self.link,
            "warn" => &mut self.warn,
            _ => return None,
        })
    }

    /// Text drawn on an `accent`/`matched` background: the theme background,
    /// or black when that is the terminal's own (unknown) colour.
    pub fn ink(&self) -> Color {
        if self.bg == Color::Reset {
            Color::Black
        } else {
            self.bg
        }
    }
}

/// A selectable theme: a built-in one, or one from the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
    pub palette: Palette,
    /// Defined (or redefined) in the config file.
    pub custom: bool,
    /// Light background.
    pub light: bool,
}

/// A colour from a config string: `#rrggbb`, `#rgb`, an ANSI name (`red`,
/// `light-blue`, `dark-gray`, …), an ANSI index `0`–`255`, or `default` /
/// `reset` for the terminal's own colour.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    if matches!(lower.as_str(), "default" | "reset" | "none" | "terminal") {
        return Some(Color::Reset);
    }
    if let Some(h) = s.strip_prefix('#') {
        let digits = |t: &str| t.len() == h.len() && t.chars().all(|c| c.is_ascii_hexdigit());
        return match h.len() {
            3 if digits(h) => {
                let v: Vec<u8> = h
                    .chars()
                    .map(|c| c.to_digit(16).unwrap_or(0) as u8 * 17)
                    .collect();
                Some(Color::Rgb(v[0], v[1], v[2]))
            }
            6 if digits(h) => {
                let n = u32::from_str_radix(h, 16).ok()?;
                Some(Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
            }
            _ => None,
        };
    }
    Color::from_str(&lower).ok()
}

/// All themes: the built-ins, then the config file's `[themes.*]`. A custom
/// theme with a built-in's name replaces it in place. Problems (unknown
/// base, role or colour) come back as warnings; the rest still applies.
pub fn catalog(cfg: &Config) -> (Vec<Theme>, Vec<String>) {
    let mut themes: Vec<Theme> = BUILTINS
        .iter()
        .map(|b| Theme {
            name: b.name.to_string(),
            palette: b.palette,
            custom: false,
            light: b.light,
        })
        .collect();
    let mut warnings = Vec::new();
    for t in &cfg.themes {
        let label = format!("[themes.{}]", t.name);
        if fold(&t.name).is_empty() {
            warnings.push(format!("{label}: the name needs a letter or digit"));
            continue;
        }
        let base = match &t.base {
            Some(b) => match find(&themes, b) {
                Some(found) => found.clone(),
                None => {
                    warnings.push(format!(
                        "{label}: unknown base theme '{b}', using {DEFAULT}"
                    ));
                    themes[0].clone()
                }
            },
            // Redefining a built-in without a base tweaks that built-in.
            None => find(&themes, &t.name)
                .filter(|f| fold(&f.name) == fold(&t.name))
                .unwrap_or(&themes[0])
                .clone(),
        };
        let mut palette = base.palette;
        for (role, value) in &t.colors {
            let Some(slot) = palette.role_mut(role) else {
                warnings.push(format!(
                    "{label}: unknown colour role '{role}' (roles: {}, base)",
                    ROLES.join(", ")
                ));
                continue;
            };
            match parse_color(value) {
                Some(c) => *slot = c,
                None => warnings.push(format!(
                    "{label}: '{value}' for {role} is not a colour (use #rrggbb, an ANSI name or 0-255)"
                )),
            }
        }
        let theme = Theme {
            name: t.name.clone(),
            palette,
            custom: true,
            light: base.light,
        };
        match themes.iter().position(|x| fold(&x.name) == fold(&t.name)) {
            Some(i) => themes[i] = theme,
            None => themes.push(theme),
        }
    }
    (themes, warnings)
}

/// Find a theme by name: loosely (`One Dark` = `one-dark`), then by
/// built-in family alias (`catppuccin` = `catppuccin-mocha`).
pub fn find<'a>(themes: &'a [Theme], name: &str) -> Option<&'a Theme> {
    let key = fold(name);
    if key.is_empty() {
        return None;
    }
    themes.iter().find(|t| fold(&t.name) == key).or_else(|| {
        let b = builtin::find(name)?;
        themes.iter().find(|t| t.name == b.name)
    })
}

/// The theme named by `env` (else `cfg.theme`, else the default) among
/// `themes`, with a warning when that name is unknown.
pub fn select(themes: &[Theme], cfg: &Config, env: Option<&str>) -> (Theme, Option<String>) {
    let (wanted, source) = match env.map(str::trim).filter(|s| !s.is_empty()) {
        Some(e) => (Some(e), THEME_ENV.to_string()),
        None => (
            cfg.theme.as_deref().filter(|s| !s.is_empty()),
            config::path().display().to_string(),
        ),
    };
    let fallback = || themes[0].clone();
    match wanted {
        None => (fallback(), None),
        Some(name) => match find(themes, name) {
            Some(t) => (t.clone(), None),
            None => (
                fallback(),
                Some(format!(
                    "unknown theme '{name}' in {source}; run `mnemoloom theme` to list themes"
                )),
            ),
        },
    }
}

/// Apply the configured theme; returns warnings for the user (bad config).
pub fn init() -> Vec<String> {
    let mut warnings = Vec::new();
    let cfg = config::load().unwrap_or_else(|e| {
        warnings.push(format!("{e:#}"));
        Config::default()
    });
    let (themes, w) = catalog(&cfg);
    warnings.extend(w);
    let env = std::env::var(THEME_ENV).ok();
    let (theme, w) = select(&themes, &cfg, env.as_deref());
    warnings.extend(w);
    apply(&theme);
    warnings
}

/// The active theme's name and palette.
#[derive(Debug, Clone)]
struct Current {
    name: Cow<'static, str>,
    palette: Palette,
}

const INITIAL: Current = Current {
    name: Cow::Borrowed(DEFAULT),
    palette: GRUVBOX_DARK,
};

// Tests run on many threads at once; each gets its own theme so a test that
// switches themes cannot recolour another test's render.
#[cfg(not(test))]
mod store {
    use std::sync::RwLock;

    use super::{Current, INITIAL};

    static CURRENT: RwLock<Current> = RwLock::new(INITIAL);

    pub(super) fn read<R>(f: impl FnOnce(&Current) -> R) -> R {
        f(&CURRENT.read().unwrap_or_else(|e| e.into_inner()))
    }

    pub(super) fn write(c: Current) {
        *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = c;
    }
}

#[cfg(test)]
mod store {
    use std::cell::RefCell;

    use super::{Current, INITIAL};

    thread_local! {
        static CURRENT: RefCell<Current> = const { RefCell::new(INITIAL) };
    }

    pub(super) fn read<R>(f: impl FnOnce(&Current) -> R) -> R {
        CURRENT.with(|c| f(&c.borrow()))
    }

    pub(super) fn write(c: Current) {
        CURRENT.with(|cur| *cur.borrow_mut() = c);
    }
}

/// Make `theme` the active one.
pub fn apply(theme: &Theme) {
    store::write(Current {
        name: Cow::Owned(theme.name.clone()),
        palette: theme.palette,
    });
}

/// The active palette.
pub fn palette() -> Palette {
    store::read(|c| c.palette)
}

/// The active theme's name.
pub fn current_name() -> String {
    store::read(|c| c.name.to_string())
}

pub fn text() -> Style {
    Style::new().fg(palette().fg)
}

pub fn dim() -> Style {
    Style::new().fg(palette().dim)
}

pub fn heading() -> Style {
    Style::new().fg(palette().accent)
}

/// Bold label such as a key in the footer.
pub fn key() -> Style {
    Style::new().fg(palette().fg).add_modifier(Modifier::BOLD)
}

pub fn border(focused: bool) -> Style {
    let p = palette();
    Style::new().fg(if focused { p.accent } else { p.border })
}

/// Inverse chip, e.g. the footer mode label.
pub fn chip() -> Style {
    let p = palette();
    Style::new()
        .fg(p.ink())
        .bg(p.accent)
        .add_modifier(Modifier::BOLD)
}

/// Highlight for the current search match.
pub fn hit_current() -> Style {
    let p = palette();
    Style::new()
        .fg(p.ink())
        .bg(p.accent)
        .add_modifier(Modifier::BOLD)
}

/// Highlight for other search matches.
pub fn hit() -> Style {
    let p = palette();
    Style::new()
        .fg(p.ink())
        .bg(p.matched)
        .add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CustomTheme;

    fn custom(name: &str, base: Option<&str>, colors: &[(&str, &str)]) -> CustomTheme {
        CustomTheme {
            name: name.into(),
            base: base.map(Into::into),
            colors: colors
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn colours() {
        assert_eq!(parse_color("#ff8000"), Some(Color::Rgb(255, 128, 0)));
        assert_eq!(parse_color(" #FF8000 "), Some(Color::Rgb(255, 128, 0)));
        assert_eq!(parse_color("#f80"), Some(Color::Rgb(255, 136, 0)));
        assert_eq!(parse_color("red"), Some(Color::Red));
        assert_eq!(parse_color("Light-Blue"), Some(Color::LightBlue));
        assert_eq!(parse_color("dark-gray"), Some(Color::DarkGray));
        assert_eq!(parse_color("208"), Some(Color::Indexed(208)));
        assert_eq!(parse_color("default"), Some(Color::Reset));
        assert_eq!(parse_color("reset"), Some(Color::Reset));
        for bad in ["#12345", "#gggggg", "#", "chartreuse-ish", "", "256"] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn every_role_is_settable() {
        let mut p = GRUVBOX_DARK;
        for role in ROLES {
            *p.role_mut(role).unwrap() = Color::Indexed(1);
        }
        assert!(p.role_mut("nope").is_none());
        assert_eq!(p.role_mut("bg-sel").copied(), Some(Color::Indexed(1)));
        let all = [
            p.bg, p.bg_sel, p.border, p.fg, p.fg_soft, p.dim, p.accent, p.matched, p.claude,
            p.codex, p.pi, p.opencode, p.agy, p.ok, p.link, p.warn,
        ];
        assert_eq!(all.len(), ROLES.len());
        assert!(all.iter().all(|c| *c == Color::Indexed(1)));
    }

    #[test]
    fn catalog_adds_and_overrides() {
        let cfg = Config {
            theme: None,
            themes: vec![
                custom(
                    "mine",
                    Some("Nord"),
                    &[("accent", "#010203"), ("bg", "default")],
                ),
                custom("nord", None, &[("warn", "red")]),
                custom("derived", Some("mine"), &[("fg", "white")]),
                custom("bad", Some("nope"), &[("colour", "red"), ("fg", "nah")]),
                custom("--", None, &[]),
            ],
        };
        let (themes, warnings) = catalog(&cfg);
        assert_eq!(
            themes.len(),
            BUILTINS.len() + 3,
            "nord is replaced, -- skipped"
        );
        let nord = builtin::find("nord").unwrap().palette;
        let mine = find(&themes, "mine").unwrap();
        assert!(mine.custom);
        assert_eq!(mine.palette.accent, Color::Rgb(1, 2, 3));
        assert_eq!(mine.palette.bg, Color::Reset);
        assert_eq!(mine.palette.ok, nord.ok);
        let tweaked = find(&themes, "nord").unwrap();
        assert!(tweaked.custom);
        assert_eq!(tweaked.palette.warn, Color::Red);
        assert_eq!(tweaked.palette.accent, nord.accent);
        let pos = themes.iter().position(|t| t.name == "nord").unwrap();
        assert_eq!(pos, 2, "a redefined built-in keeps its place");
        let derived = find(&themes, "derived").unwrap();
        assert_eq!(derived.palette.accent, Color::Rgb(1, 2, 3));
        assert_eq!(derived.palette.fg, Color::White);
        let bad = find(&themes, "bad").unwrap();
        assert_eq!(bad.palette, GRUVBOX_DARK);
        assert_eq!(warnings.len(), 4, "{warnings:#?}");
        assert!(warnings[0].contains("unknown base theme 'nope'"));
        assert!(warnings[1].contains("unknown colour role 'colour'"));
        assert!(warnings[2].contains("'nah' for fg"));
        assert!(warnings[3].contains("[themes.--]"));
    }

    #[test]
    fn selection_precedence() {
        let cfg = Config {
            theme: Some("dracula".into()),
            themes: vec![custom("mine", Some("nord"), &[])],
        };
        let (themes, _) = catalog(&cfg);
        assert_eq!(select(&themes, &cfg, None).0.name, "dracula");
        assert_eq!(select(&themes, &cfg, Some("One Dark")).0.name, "one-dark");
        assert_eq!(select(&themes, &cfg, Some("  ")).0.name, "dracula");
        assert_eq!(select(&themes, &cfg, Some("MINE")).0.name, "mine");
        assert_eq!(
            select(&themes, &cfg, Some("catppuccin")).0.name,
            "catppuccin-mocha"
        );
        let (t, w) = select(&themes, &cfg, Some("nope"));
        assert_eq!(t.name, DEFAULT);
        assert!(
            w.unwrap()
                .contains("unknown theme 'nope' in MNEMOLOOM_THEME")
        );
        let empty = Config::default();
        assert_eq!(select(&themes, &empty, None), (themes[0].clone(), None));
    }

    #[test]
    fn apply_switches_the_active_palette() {
        assert_eq!(current_name(), DEFAULT);
        assert_eq!(palette(), GRUVBOX_DARK);
        let (themes, _) = catalog(&Config::default());
        let nord = find(&themes, "nord").unwrap();
        apply(nord);
        assert_eq!(current_name(), "nord");
        assert_eq!(heading().fg, Some(nord.palette.accent));
        assert_eq!(hit().fg, Some(nord.palette.bg));
        apply(find(&themes, "terminal").unwrap());
        assert_eq!(
            chip().fg,
            Some(Color::Black),
            "readable on the terminal background"
        );
        apply(&themes[0]);
    }
}
