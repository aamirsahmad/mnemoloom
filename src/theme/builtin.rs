//! Built-in colour themes. Each maps a popular editor/terminal scheme onto
//! the TUI [`Palette`] roles; the first one is the default.

use ratatui::style::Color;

use super::Palette;

/// `0xRRGGBB` as a true colour.
const fn hex(c: u32) -> Color {
    Color::Rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// A named built-in theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Builtin {
    /// Canonical, kebab-case name (what the config file and CLI take).
    pub name: &'static str,
    /// Light background (for listing only).
    pub light: bool,
    pub palette: Palette,
}

/// Name of the default theme.
pub const DEFAULT: &str = "gruvbox-dark";

/// The original "Inspector" palette.
pub const GRUVBOX_DARK: Palette = Palette {
    bg: hex(0x1d2021),
    bg_sel: hex(0x32302f),
    border: hex(0x504945),
    fg: hex(0xebdbb2),
    fg_soft: hex(0xd5c4a1),
    dim: hex(0xa89984),
    accent: hex(0xfabd2f),
    matched: hex(0xfe8019),
    claude: hex(0xfe8019),
    codex: hex(0x83a598),
    pi: hex(0xd3869b),
    opencode: hex(0xb8bb26),
    agy: hex(0x7cb8ff),
    ok: hex(0x8ec07c),
    link: hex(0x83a598),
    warn: hex(0xfb4934),
};

const GRUVBOX_LIGHT: Palette = Palette {
    bg: hex(0xfbf1c7),
    bg_sel: hex(0xebdbb2),
    border: hex(0xbdae93),
    fg: hex(0x3c3836),
    fg_soft: hex(0x504945),
    dim: hex(0x7c6f64),
    accent: hex(0xb57614),
    matched: hex(0xaf3a03),
    claude: hex(0xaf3a03),
    codex: hex(0x427b58),
    pi: hex(0x8f3f71),
    opencode: hex(0x79740e),
    agy: hex(0x076678),
    ok: hex(0x79740e),
    link: hex(0x076678),
    warn: hex(0x9d0006),
};

const NORD: Palette = Palette {
    bg: hex(0x2e3440),
    bg_sel: hex(0x3b4252),
    border: hex(0x4c566a),
    fg: hex(0xeceff4),
    fg_soft: hex(0xd8dee9),
    dim: hex(0x7b88a1),
    accent: hex(0x88c0d0),
    matched: hex(0xebcb8b),
    claude: hex(0xd08770),
    codex: hex(0x8fbcbb),
    pi: hex(0xb48ead),
    opencode: hex(0xa3be8c),
    agy: hex(0x81a1c1),
    ok: hex(0xa3be8c),
    link: hex(0x81a1c1),
    warn: hex(0xbf616a),
};

const ONE_DARK: Palette = Palette {
    bg: hex(0x282c34),
    bg_sel: hex(0x3e4451),
    border: hex(0x4b5263),
    fg: hex(0xabb2bf),
    fg_soft: hex(0x9da5b4),
    dim: hex(0x7f848e),
    accent: hex(0x61afef),
    matched: hex(0xe5c07b),
    claude: hex(0xd19a66),
    codex: hex(0x56b6c2),
    pi: hex(0xc678dd),
    opencode: hex(0x98c379),
    agy: hex(0x61afef),
    ok: hex(0x98c379),
    link: hex(0x56b6c2),
    warn: hex(0xe06c75),
};

const CATPPUCCIN_MOCHA: Palette = Palette {
    bg: hex(0x1e1e2e),
    bg_sel: hex(0x313244),
    border: hex(0x45475a),
    fg: hex(0xcdd6f4),
    fg_soft: hex(0xbac2de),
    dim: hex(0x7f849c),
    accent: hex(0xcba6f7),
    matched: hex(0xfab387),
    claude: hex(0xfab387),
    codex: hex(0x94e2d5),
    pi: hex(0xf5c2e7),
    opencode: hex(0xa6e3a1),
    agy: hex(0x89b4fa),
    ok: hex(0xa6e3a1),
    link: hex(0x74c7ec),
    warn: hex(0xf38ba8),
};

const CATPPUCCIN_MACCHIATO: Palette = Palette {
    bg: hex(0x24273a),
    bg_sel: hex(0x363a4f),
    border: hex(0x494d64),
    fg: hex(0xcad3f5),
    fg_soft: hex(0xb8c0e0),
    dim: hex(0x8087a2),
    accent: hex(0xc6a0f6),
    matched: hex(0xf5a97f),
    claude: hex(0xf5a97f),
    codex: hex(0x8bd5ca),
    pi: hex(0xf5bde6),
    opencode: hex(0xa6da95),
    agy: hex(0x8aadf4),
    ok: hex(0xa6da95),
    link: hex(0x7dc4e4),
    warn: hex(0xed8796),
};

const CATPPUCCIN_FRAPPE: Palette = Palette {
    bg: hex(0x303446),
    bg_sel: hex(0x414559),
    border: hex(0x51576d),
    fg: hex(0xc6d0f5),
    fg_soft: hex(0xb5bfe2),
    dim: hex(0x838ba7),
    accent: hex(0xca9ee6),
    matched: hex(0xef9f76),
    claude: hex(0xef9f76),
    codex: hex(0x81c8be),
    pi: hex(0xf4b8e4),
    opencode: hex(0xa6d189),
    agy: hex(0x8caaee),
    ok: hex(0xa6d189),
    link: hex(0x85c1dc),
    warn: hex(0xe78284),
};

const CATPPUCCIN_LATTE: Palette = Palette {
    bg: hex(0xeff1f5),
    bg_sel: hex(0xccd0da),
    border: hex(0xbcc0cc),
    fg: hex(0x4c4f69),
    fg_soft: hex(0x5c5f77),
    dim: hex(0x8c8fa1),
    accent: hex(0x8839ef),
    matched: hex(0xfe640b),
    claude: hex(0xfe640b),
    codex: hex(0x179299),
    pi: hex(0xea76cb),
    opencode: hex(0x40a02b),
    agy: hex(0x1e66f5),
    ok: hex(0x40a02b),
    link: hex(0x209fb5),
    warn: hex(0xd20f39),
};

const DRACULA: Palette = Palette {
    bg: hex(0x282a36),
    bg_sel: hex(0x44475a),
    border: hex(0x565970),
    fg: hex(0xf8f8f2),
    fg_soft: hex(0xe2e2dc),
    dim: hex(0x7b85b4),
    accent: hex(0xbd93f9),
    matched: hex(0xffb86c),
    claude: hex(0xffb86c),
    codex: hex(0x8be9fd),
    pi: hex(0xff79c6),
    opencode: hex(0x50fa7b),
    agy: hex(0xbd93f9),
    ok: hex(0x50fa7b),
    link: hex(0x8be9fd),
    warn: hex(0xff5555),
};

const TOKYO_NIGHT: Palette = Palette {
    bg: hex(0x1a1b26),
    bg_sel: hex(0x292e42),
    border: hex(0x3b4261),
    fg: hex(0xc0caf5),
    fg_soft: hex(0xa9b1d6),
    dim: hex(0x737aa2),
    accent: hex(0x7aa2f7),
    matched: hex(0xff9e64),
    claude: hex(0xff9e64),
    codex: hex(0x7dcfff),
    pi: hex(0xbb9af7),
    opencode: hex(0x9ece6a),
    agy: hex(0x2ac3de),
    ok: hex(0x9ece6a),
    link: hex(0x7dcfff),
    warn: hex(0xf7768e),
};

const TOKYO_NIGHT_STORM: Palette = Palette {
    bg: hex(0x24283b),
    bg_sel: hex(0x2f354f),
    border: hex(0x414868),
    ..TOKYO_NIGHT
};

const TOKYO_NIGHT_DAY: Palette = Palette {
    bg: hex(0xe1e2e7),
    bg_sel: hex(0xc4c8da),
    border: hex(0xa8aecb),
    fg: hex(0x3760bf),
    fg_soft: hex(0x6172b0),
    dim: hex(0x848cb5),
    accent: hex(0x2e7de9),
    matched: hex(0xb15c00),
    claude: hex(0xb15c00),
    codex: hex(0x007197),
    pi: hex(0x9854f1),
    opencode: hex(0x587539),
    agy: hex(0x188092),
    ok: hex(0x587539),
    link: hex(0x007197),
    warn: hex(0xf52a65),
};

const SOLARIZED_DARK: Palette = Palette {
    bg: hex(0x002b36),
    bg_sel: hex(0x073642),
    border: hex(0x2c4f58),
    fg: hex(0x93a1a1),
    fg_soft: hex(0x839496),
    dim: hex(0x657b83),
    accent: hex(0xb58900),
    matched: hex(0xcb4b16),
    claude: hex(0xcb4b16),
    codex: hex(0x2aa198),
    pi: hex(0xd33682),
    opencode: hex(0x859900),
    agy: hex(0x268bd2),
    ok: hex(0x859900),
    link: hex(0x268bd2),
    warn: hex(0xdc322f),
};

const SOLARIZED_LIGHT: Palette = Palette {
    bg: hex(0xfdf6e3),
    bg_sel: hex(0xeee8d5),
    border: hex(0x93a1a1),
    fg: hex(0x586e75),
    fg_soft: hex(0x657b83),
    dim: hex(0x93a1a1),
    ..SOLARIZED_DARK
};

const EVERFOREST_DARK: Palette = Palette {
    bg: hex(0x2d353b),
    bg_sel: hex(0x3d484d),
    border: hex(0x475258),
    fg: hex(0xd3c6aa),
    fg_soft: hex(0xc0b498),
    dim: hex(0x859289),
    accent: hex(0xdbbc7f),
    matched: hex(0xe69875),
    claude: hex(0xe69875),
    codex: hex(0x7fbbb3),
    pi: hex(0xd699b6),
    opencode: hex(0xa7c080),
    agy: hex(0x83c092),
    ok: hex(0xa7c080),
    link: hex(0x7fbbb3),
    warn: hex(0xe67e80),
};

const EVERFOREST_LIGHT: Palette = Palette {
    bg: hex(0xfdf6e3),
    bg_sel: hex(0xefebd4),
    border: hex(0xe0dcc7),
    fg: hex(0x5c6a72),
    fg_soft: hex(0x708089),
    dim: hex(0x939f91),
    accent: hex(0xdfa000),
    matched: hex(0xf57d26),
    claude: hex(0xf57d26),
    codex: hex(0x3a94c5),
    pi: hex(0xdf69ba),
    opencode: hex(0x8da101),
    agy: hex(0x35a77c),
    ok: hex(0x8da101),
    link: hex(0x3a94c5),
    warn: hex(0xf85552),
};

const ROSE_PINE: Palette = Palette {
    bg: hex(0x191724),
    bg_sel: hex(0x26233a),
    border: hex(0x403d52),
    fg: hex(0xe0def4),
    fg_soft: hex(0xc8c5df),
    dim: hex(0x908caa),
    accent: hex(0xc4a7e7),
    matched: hex(0xf6c177),
    claude: hex(0xf6c177),
    codex: hex(0x9ccfd8),
    pi: hex(0xebbcba),
    opencode: hex(0x3e8fb0),
    agy: hex(0xc4a7e7),
    ok: hex(0x9ccfd8),
    link: hex(0x9ccfd8),
    warn: hex(0xeb6f92),
};

const ROSE_PINE_MOON: Palette = Palette {
    bg: hex(0x232136),
    bg_sel: hex(0x393552),
    border: hex(0x44415a),
    pi: hex(0xea9a97),
    ..ROSE_PINE
};

const ROSE_PINE_DAWN: Palette = Palette {
    bg: hex(0xfaf4ed),
    bg_sel: hex(0xf2e9e1),
    border: hex(0xdfdad9),
    fg: hex(0x575279),
    fg_soft: hex(0x6e6a86),
    dim: hex(0x9893a5),
    accent: hex(0x907aa9),
    matched: hex(0xea9d34),
    claude: hex(0xea9d34),
    codex: hex(0x56949f),
    pi: hex(0xd7827e),
    opencode: hex(0x286983),
    agy: hex(0x907aa9),
    ok: hex(0x56949f),
    link: hex(0x286983),
    warn: hex(0xb4637a),
};

const KANAGAWA: Palette = Palette {
    bg: hex(0x1f1f28),
    bg_sel: hex(0x2a2a37),
    border: hex(0x54546d),
    fg: hex(0xdcd7ba),
    fg_soft: hex(0xc8c093),
    dim: hex(0x727169),
    accent: hex(0xe6c384),
    matched: hex(0xffa066),
    claude: hex(0xffa066),
    codex: hex(0x7fb4ca),
    pi: hex(0xd27e99),
    opencode: hex(0x98bb6c),
    agy: hex(0x7e9cd8),
    ok: hex(0x98bb6c),
    link: hex(0x7fb4ca),
    warn: hex(0xe46876),
};

const MONOKAI: Palette = Palette {
    bg: hex(0x272822),
    bg_sel: hex(0x3e3d32),
    border: hex(0x49483e),
    fg: hex(0xf8f8f2),
    fg_soft: hex(0xcfcfc2),
    dim: hex(0x90908a),
    accent: hex(0xe6db74),
    matched: hex(0xfd971f),
    claude: hex(0xfd971f),
    codex: hex(0x66d9ef),
    pi: hex(0xae81ff),
    opencode: hex(0xa6e22e),
    agy: hex(0x78dce8),
    ok: hex(0xa6e22e),
    link: hex(0x66d9ef),
    warn: hex(0xf92672),
};

const GITHUB_DARK: Palette = Palette {
    bg: hex(0x0d1117),
    bg_sel: hex(0x161b22),
    border: hex(0x30363d),
    fg: hex(0xe6edf3),
    fg_soft: hex(0xc9d1d9),
    dim: hex(0x8b949e),
    accent: hex(0x58a6ff),
    matched: hex(0xd29922),
    claude: hex(0xf0883e),
    codex: hex(0x39c5cf),
    pi: hex(0xbc8cff),
    opencode: hex(0x3fb950),
    agy: hex(0x79c0ff),
    ok: hex(0x3fb950),
    link: hex(0x58a6ff),
    warn: hex(0xf85149),
};

const GITHUB_LIGHT: Palette = Palette {
    bg: hex(0xffffff),
    bg_sel: hex(0xeaeef2),
    border: hex(0xd0d7de),
    fg: hex(0x1f2328),
    fg_soft: hex(0x424a53),
    dim: hex(0x656d76),
    accent: hex(0x0969da),
    matched: hex(0xbf8700),
    claude: hex(0xbc4c00),
    codex: hex(0x1b7c83),
    pi: hex(0x8250df),
    opencode: hex(0x1a7f37),
    agy: hex(0x0550ae),
    ok: hex(0x1a7f37),
    link: hex(0x0969da),
    warn: hex(0xcf222e),
};

const AYU_DARK: Palette = Palette {
    bg: hex(0x0b0e14),
    bg_sel: hex(0x1b1f29),
    border: hex(0x2d3240),
    fg: hex(0xbfbdb6),
    fg_soft: hex(0xacaaa3),
    dim: hex(0x6c7380),
    accent: hex(0xe6b450),
    matched: hex(0xff8f40),
    claude: hex(0xff8f40),
    codex: hex(0x95e6cb),
    pi: hex(0xd2a6ff),
    opencode: hex(0xaad94c),
    agy: hex(0x59c2ff),
    ok: hex(0xaad94c),
    link: hex(0x59c2ff),
    warn: hex(0xf07178),
};

/// The terminal's own colours: default background and foreground plus the
/// 16 ANSI colours, so the TUI follows the terminal's scheme (and its
/// transparency).
const TERMINAL: Palette = Palette {
    bg: Color::Reset,
    bg_sel: Color::DarkGray,
    border: Color::DarkGray,
    fg: Color::Reset,
    fg_soft: Color::Reset,
    dim: Color::Gray,
    accent: Color::Yellow,
    matched: Color::LightRed,
    claude: Color::LightRed,
    codex: Color::Cyan,
    pi: Color::Magenta,
    opencode: Color::Green,
    agy: Color::Blue,
    ok: Color::Green,
    link: Color::Cyan,
    warn: Color::Red,
};

const fn dark(name: &'static str, palette: Palette) -> Builtin {
    Builtin {
        name,
        light: false,
        palette,
    }
}

const fn light(name: &'static str, palette: Palette) -> Builtin {
    Builtin {
        name,
        light: true,
        palette,
    }
}

/// Every built-in theme, the default first.
pub const BUILTINS: &[Builtin] = &[
    dark(DEFAULT, GRUVBOX_DARK),
    light("gruvbox-light", GRUVBOX_LIGHT),
    dark("nord", NORD),
    dark("one-dark", ONE_DARK),
    dark("catppuccin-mocha", CATPPUCCIN_MOCHA),
    dark("catppuccin-macchiato", CATPPUCCIN_MACCHIATO),
    dark("catppuccin-frappe", CATPPUCCIN_FRAPPE),
    light("catppuccin-latte", CATPPUCCIN_LATTE),
    dark("dracula", DRACULA),
    dark("tokyo-night", TOKYO_NIGHT),
    dark("tokyo-night-storm", TOKYO_NIGHT_STORM),
    light("tokyo-night-day", TOKYO_NIGHT_DAY),
    dark("solarized-dark", SOLARIZED_DARK),
    light("solarized-light", SOLARIZED_LIGHT),
    dark("everforest-dark", EVERFOREST_DARK),
    light("everforest-light", EVERFOREST_LIGHT),
    dark("rose-pine", ROSE_PINE),
    dark("rose-pine-moon", ROSE_PINE_MOON),
    light("rose-pine-dawn", ROSE_PINE_DAWN),
    dark("kanagawa", KANAGAWA),
    dark("monokai", MONOKAI),
    dark("github-dark", GITHUB_DARK),
    light("github-light", GITHUB_LIGHT),
    dark("ayu-dark", AYU_DARK),
    dark("terminal", TERMINAL),
];

/// Family names that pick a default variant.
const ALIASES: &[(&str, &str)] = &[
    ("gruvbox", "gruvbox-dark"),
    ("catppuccin", "catppuccin-mocha"),
    ("catpuccin", "catppuccin-mocha"),
    ("catpuccinmocha", "catppuccin-mocha"),
    ("catpuccinmacchiato", "catppuccin-macchiato"),
    ("catpuccinfrappe", "catppuccin-frappe"),
    ("catpuccinlatte", "catppuccin-latte"),
    ("solarized", "solarized-dark"),
    ("everforest", "everforest-dark"),
    ("github", "github-dark"),
    ("ayu", "ayu-dark"),
    ("onedarkpro", "one-dark"),
    ("rosepinemain", "rose-pine"),
    ("ansi", "terminal"),
    ("default", DEFAULT),
];

/// Lowercase letters and digits only: `One Dark`, `one_dark` and `onedark`
/// all match `one-dark`.
pub fn fold(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Look up a built-in theme by name, loosely (see [`fold`]) and by family
/// alias (`catppuccin` = `catppuccin-mocha`).
pub fn find(name: &str) -> Option<&'static Builtin> {
    let key = fold(name);
    if key.is_empty() {
        return None;
    }
    let key = ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map_or(key, |(_, target)| fold(target));
    BUILTINS.iter().find(|b| fold(b.name) == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_kebab_case() {
        let mut seen = std::collections::HashSet::new();
        for b in BUILTINS {
            assert!(seen.insert(fold(b.name)), "duplicate {}", b.name);
            assert!(
                b.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{}",
                b.name
            );
        }
        assert_eq!(BUILTINS[0].name, DEFAULT);
    }

    #[test]
    fn loose_lookup_and_aliases() {
        assert_eq!(find("nord").unwrap().name, "nord");
        assert_eq!(find("One Dark").unwrap().name, "one-dark");
        assert_eq!(find("onedark").unwrap().name, "one-dark");
        assert_eq!(find("one_dark").unwrap().name, "one-dark");
        assert_eq!(find("Catppuccin").unwrap().name, "catppuccin-mocha");
        assert_eq!(find("catppuccin-latte").unwrap().name, "catppuccin-latte");
        assert_eq!(find("tokyonight").unwrap().name, "tokyo-night");
        assert_eq!(find("rosé-pine"), None, "non-ASCII letters are dropped");
        assert_eq!(find("rose pine").unwrap().name, "rose-pine");
        assert_eq!(find("default").unwrap().name, DEFAULT);
        assert_eq!(find("nope"), None);
        assert_eq!(find(" - "), None);
    }

    #[test]
    fn aliases_point_at_builtins() {
        for (alias, target) in ALIASES {
            assert!(find(target).is_some(), "{alias} -> {target}");
            assert!(
                !BUILTINS.iter().any(|b| fold(b.name) == *alias),
                "alias {alias} shadows a theme"
            );
        }
    }
}
