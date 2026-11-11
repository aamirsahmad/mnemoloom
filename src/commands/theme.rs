//! `mnemoloom theme [NAME] [--path]`: list themes or set the active theme in
//! the config file.

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use ratatui::style::Color;

use crate::cli::ThemeArgs;
use crate::config::{self, Config};
use crate::exit::Exit;
use crate::theme::builtin::DEFAULT;
use crate::theme::{self, Palette, THEME_ENV};

pub fn run(args: &ThemeArgs) -> anyhow::Result<ExitCode> {
    if args.path {
        println!("{}", config::path().display());
        return Ok(ExitCode::SUCCESS);
    }
    let cfg_path = config::path();
    let env = std::env::var(THEME_ENV).ok();
    let colour = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    match &args.name {
        Some(name) => {
            let msg = set_theme_at(&cfg_path, name)?;
            println!("{msg}");
        }
        None => {
            let (out, warnings) = list_themes_at(&cfg_path, env.as_deref(), colour)?;
            for w in warnings {
                eprintln!("Warning: {w}");
            }
            print!("{out}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn set_theme_at(cfg_path: &Path, name: &str) -> anyhow::Result<String> {
    let cfg = config::load_from(cfg_path)?;
    let (themes, warnings) = theme::catalog(&cfg);
    for w in warnings {
        eprintln!("Warning: {w}");
    }
    let Some(found) = theme::find(&themes, name) else {
        return Err(Exit::fail(format!(
            "Error: Unknown theme '{name}'. Run `mnemoloom theme` to list themes."
        ))
        .into());
    };
    let canonical = found.name.clone();
    config::save_theme_to(cfg_path, &canonical)?;
    Ok(format!(
        "Theme set to '{canonical}' ({})",
        cfg_path.display()
    ))
}

fn list_themes_at(
    cfg_path: &Path,
    env: Option<&str>,
    colour: bool,
) -> anyhow::Result<(String, Vec<String>)> {
    let cfg: Config = config::load_from(cfg_path)?;
    let (themes, mut warnings) = theme::catalog(&cfg);
    let (active, sel_warn) = theme::select(&themes, &cfg, env);
    warnings.extend(sel_warn);
    let width = themes.iter().map(|t| t.name.len()).max().unwrap_or(20);
    let mut out = String::new();
    for t in &themes {
        let mark = if t.name == active.name { '*' } else { ' ' };
        let kind = if t.light { "light" } else { "dark " };
        let mut tags = Vec::new();
        if t.name == DEFAULT && !t.custom {
            tags.push("default");
        }
        if t.custom {
            tags.push("custom");
        }
        let extra = if tags.is_empty() {
            String::new()
        } else {
            format!(" ({})", tags.join(", "))
        };
        let swatch = if colour {
            format!("  {}", ansi_swatches(&t.palette))
        } else {
            String::new()
        };
        out.push_str(&format!(
            " {mark} {:<width$}  {kind}{swatch}{extra}\n",
            t.name,
            width = width
        ));
    }
    out.push_str(&format!(
        "\nConfig: {} (or ${THEME_ENV}, or press ctrl-o in a TUI)\n",
        cfg_path.display()
    ));
    Ok((out, warnings))
}

fn ansi_swatches(p: &Palette) -> String {
    let cols = [p.accent, p.ok, p.link, p.pi, p.warn];
    let mut s = String::new();
    for (i, c) in cols.into_iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        match c {
            Color::Rgb(r, g, b) => s.push_str(&format!("\x1b[38;2;{r};{g};{b}m●\x1b[0m")),
            _ => s.push('●'),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_and_set_themes() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.toml");

        let (list, warnings) = list_themes_at(&cfg_path, None, false).unwrap();
        assert!(warnings.is_empty());
        assert!(list.contains("* gruvbox-dark"));
        assert!(list.contains("  nord"));
        assert!(list.contains("  one-dark"));
        assert!(list.contains("  catppuccin-mocha"));

        // Loose name and alias resolution when setting.
        let msg = set_theme_at(&cfg_path, "One Dark").unwrap();
        assert!(msg.contains("Theme set to 'one-dark'"), "{msg}");
        assert_eq!(
            config::load_from(&cfg_path).unwrap().theme.as_deref(),
            Some("one-dark")
        );
        let (list, _) = list_themes_at(&cfg_path, None, false).unwrap();
        assert!(list.contains("* one-dark"), "{list}");

        let msg = set_theme_at(&cfg_path, "catpuccin").unwrap();
        assert!(msg.contains("Theme set to 'catppuccin-mocha'"), "{msg}");

        // Env override is reflected in the active marker.
        let (list, _) = list_themes_at(&cfg_path, Some("nord"), true).unwrap();
        assert!(list.contains("* nord"), "{list}");
        assert!(list.contains("\x1b[38;2;"), "colour swatches included");

        // Unknown theme fails cleanly.
        let err = set_theme_at(&cfg_path, "nope").unwrap_err();
        let msg = err.downcast::<Exit>().unwrap().message.unwrap();
        assert!(msg.contains("Unknown theme 'nope'"), "{msg}");
    }
}
