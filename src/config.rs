//! The user config file: `$MNEMOLOOM_CONFIG`, else
//! `$XDG_CONFIG_HOME/mnemoloom/config.toml`, else
//! `~/.config/mnemoloom/config.toml`.
//!
//! ```toml
//! theme = "nord"
//!
//! # A custom theme: a built-in base plus colour overrides.
//! [themes.my-nord]
//! base = "nord"
//! bg = "default"      # the terminal background
//! accent = "#ebcb8b"
//! ```
//!
//! A missing file is an empty config. [`save_theme`] edits the file in
//! place, keeping the user's comments and other settings.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use toml_edit::{DocumentMut, Item, Value};

use crate::session::homes::{expand_tilde, user_home};
use crate::session::transcript::write_atomic;

/// Env override for the config file path.
pub const CONFIG_ENV: &str = "MNEMOLOOM_CONFIG";

/// A `[themes.<name>]` table: raw strings, validated by [`crate::theme`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomTheme {
    pub name: String,
    /// Built-in (or earlier custom) theme to start from.
    pub base: Option<String>,
    /// `(role, colour)` overrides in file order.
    pub colors: Vec<(String, String)>,
}

/// The parsed config file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// `theme = "<name>"`.
    pub theme: Option<String>,
    pub themes: Vec<CustomTheme>,
}

/// The config file path (see the module docs); it may not exist.
pub fn path() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    path_from(env(CONFIG_ENV), env("XDG_CONFIG_HOME"), &user_home())
}

fn path_from(explicit: Option<OsString>, xdg: Option<OsString>, home: &Path) -> PathBuf {
    if let Some(p) = explicit {
        return expand_tilde(Path::new(&p));
    }
    xdg.map(PathBuf::from)
        .filter(|x| x.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("mnemoloom")
        .join("config.toml")
}

/// Load the config file at [`path`].
pub fn load() -> anyhow::Result<Config> {
    load_from(&path())
}

/// Load `path`; a missing file is the default config.
pub fn load_from(path: &Path) -> anyhow::Result<Config> {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text).with_context(|| format!("{}", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Parse config text.
pub fn parse(text: &str) -> anyhow::Result<Config> {
    let doc: DocumentMut = text.parse()?;
    let mut cfg = Config::default();
    if let Some(item) = doc.get("theme") {
        match item.as_str() {
            Some(s) => cfg.theme = Some(s.trim().to_string()),
            None => bail!("`theme` must be a string, e.g. theme = \"nord\""),
        }
    }
    if let Some(item) = doc.get("themes") {
        let Some(table) = item.as_table_like() else {
            bail!("`themes` must be a table of [themes.<name>] tables");
        };
        for (name, body) in table.iter() {
            let Some(body) = body.as_table_like() else {
                bail!("`themes.{name}` must be a table");
            };
            let mut t = CustomTheme {
                name: name.to_string(),
                ..CustomTheme::default()
            };
            for (key, v) in body.iter() {
                let Some(s) = v.as_str() else {
                    bail!("`themes.{name}.{key}` must be a string");
                };
                if key == "base" {
                    t.base = Some(s.trim().to_string());
                } else {
                    t.colors.push((key.to_string(), s.trim().to_string()));
                }
            }
            cfg.themes.push(t);
        }
    }
    Ok(cfg)
}

const NEW_FILE: &str = "\
# mnemoloom configuration.
# Themes: run `mnemoloom theme` to list them, or press ctrl-o in a TUI.
theme = \"gruvbox-dark\"
";

/// Set `theme = "<name>"` in the config file at [`path`]; returns the path.
pub fn save_theme(name: &str) -> anyhow::Result<PathBuf> {
    let p = path();
    save_theme_to(&p, name)?;
    Ok(p)
}

/// Set `theme = "<name>"` in `path`, creating it (and its directory) when
/// missing. Everything else in the file is kept as written; a file that
/// does not parse is left alone.
pub fn save_theme_to(path: &Path, name: &str) -> anyhow::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => NEW_FILE.to_string(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let mut doc: DocumentMut = text
        .parse()
        .with_context(|| format!("{} is not valid TOML; not changing it", path.display()))?;
    match doc.get_mut("theme") {
        // Keep the comments around an existing value.
        Some(Item::Value(v)) => {
            let decor = v.decor().clone();
            *v = Value::from(name);
            *v.decor_mut() = decor;
        }
        _ => doc["theme"] = toml_edit::value(name),
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    write_atomic(path, doc.to_string().as_bytes(), None)
        .with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_precedence() {
        let home = Path::new("/h");
        let p = |e: Option<&str>, x: Option<&str>| {
            path_from(e.map(Into::into), x.map(Into::into), home)
        };
        assert_eq!(p(Some("/c/m.toml"), Some("/x")), PathBuf::from("/c/m.toml"));
        assert_eq!(
            p(None, Some("/x")),
            PathBuf::from("/x/mnemoloom/config.toml")
        );
        assert_eq!(
            p(None, Some("rel")),
            PathBuf::from("/h/.config/mnemoloom/config.toml"),
            "relative XDG paths are ignored"
        );
        assert_eq!(
            p(None, None),
            PathBuf::from("/h/.config/mnemoloom/config.toml")
        );
    }

    #[test]
    fn parses_theme_and_custom_themes() {
        let cfg = parse(
            r##"
            theme = " nord "
            [themes.mine]
            base = "dracula"
            accent = "#ff0000"
            bg = "default"
            [themes."other one"]
            fg = "white"
            "##,
        )
        .unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("nord"));
        assert_eq!(cfg.themes.len(), 2);
        assert_eq!(cfg.themes[0].name, "mine");
        assert_eq!(cfg.themes[0].base.as_deref(), Some("dracula"));
        assert_eq!(
            cfg.themes[0].colors,
            vec![
                ("accent".to_string(), "#ff0000".to_string()),
                ("bg".to_string(), "default".to_string())
            ]
        );
        assert_eq!(cfg.themes[1].name, "other one");
        assert_eq!(cfg.themes[1].base, None);
        assert_eq!(parse("").unwrap(), Config::default());
        // Inline tables work too.
        let cfg = parse("themes = { x = { base = \"nord\" } }").unwrap();
        assert_eq!(cfg.themes[0].base.as_deref(), Some("nord"));
    }

    #[test]
    fn rejects_bad_shapes() {
        for bad in [
            "theme = 3",
            "themes = 1",
            "themes = { x = 1 }",
            "[themes.x]\naccent = 1",
            "theme = ",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_from(&dir.path().join("nope.toml")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn save_creates_then_edits_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/dir/config.toml");
        save_theme_to(&p, "nord").unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("# mnemoloom configuration."), "{text}");
        assert_eq!(load_from(&p).unwrap().theme.as_deref(), Some("nord"));

        fs::write(
            &p,
            "# top comment\ntheme = \"nord\" # my pick\n\n[themes.x]\n# keep me\nbase = \"nord\"\n",
        )
        .unwrap();
        save_theme_to(&p, "one-dark").unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert_eq!(
            text,
            "# top comment\ntheme = \"one-dark\" # my pick\n\n[themes.x]\n# keep me\nbase = \"nord\"\n"
        );
    }

    #[test]
    fn save_adds_theme_above_tables() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        fs::write(&p, "[themes.x]\nbase = \"nord\"\n").unwrap();
        save_theme_to(&p, "x").unwrap();
        let cfg = load_from(&p).unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("x"));
        assert_eq!(cfg.themes.len(), 1);
    }

    #[test]
    fn save_refuses_to_clobber_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        fs::write(&p, "theme = = broken").unwrap();
        assert!(save_theme_to(&p, "nord").is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "theme = = broken");
    }
}
