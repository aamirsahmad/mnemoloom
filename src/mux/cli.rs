//! Command-line grammar, including the implicit `pick` subcommand.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use mnemoloom::session::homes::expand_tilde;

use super::filters::{SortKey, View};

#[derive(Parser, Debug)]
#[command(
    name = "mnemoloom mux",
    bin_name = "mnemoloom mux",
    version,
    disable_help_subcommand = true,
    about = "See and jump between coding agents running in tmux panes."
)]
pub struct Cli {
    /// Seconds a cached scan stays usable (default: 30)
    #[arg(long, global = true, value_name = "SECS", default_value = "30", hide_default_value = true,
          value_parser = finite_seconds)]
    pub max_age: f64,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Interactive picker (default)
    Pick(ViewArgs),
    /// Print all live agents
    List(ListArgs),
    /// Refresh the cache
    Scan,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Machine-readable output
    #[arg(long)]
    pub json: bool,
    /// Use the cache if fresh
    #[arg(long)]
    pub cached: bool,
    /// Disable colour
    #[arg(long)]
    pub no_color: bool,
    #[command(flatten)]
    pub view: ViewArgs,
}

#[derive(Args, Debug)]
pub struct ViewArgs {
    /// Exact repository name, including linked Git worktrees
    #[arg(long, visible_alias = "project", value_name = "NAME")]
    pub repo: Option<String>,
    /// Only agents in this directory or its subdirectories
    #[arg(long, value_name = "PATH", value_parser = directory)]
    pub dir: Option<PathBuf>,
    /// Show waiting agents with old known submitted input only
    #[arg(long)]
    pub dormant: bool,
    /// Input age in hours above which waiting agents are dormant (72)
    #[arg(long, value_name = "H", default_value = "72", hide_default_value = true,
          value_parser = finite_seconds)]
    pub dormant_hours: f64,
    /// Sort by current state or oldest known submitted input
    #[arg(long, value_enum, default_value_t = SortKey::State)]
    pub sort: SortKey,
}

impl From<ViewArgs> for View {
    fn from(a: ViewArgs) -> Self {
        View {
            repo: a.repo,
            dir: a.dir,
            dormant: a.dormant,
            dormant_hours: a.dormant_hours,
            sort: a.sort,
        }
    }
}

const VALUE_OPTIONS: [&str; 6] = [
    "--repo",
    "--project",
    "--dir",
    "--max-age",
    "--dormant-hours",
    "--sort",
];
const INFO_FLAGS: [&str; 4] = ["-h", "--help", "-V", "--version"];

/// Parse `args` (the args after `mnemoloom mux`).
pub fn parse(args: Vec<OsString>) -> Result<Cli, clap::Error> {
    let argv = std::iter::once(OsString::from("mnemoloom mux")).chain(with_default_command(args));
    let matches = Cli::command().try_get_matches_from(argv)?;
    Cli::from_arg_matches(&matches)
}

/// Insert `pick` when no subcommand is named: no args, or only options
/// (other than help/version) before the first positional. A positional
/// that is not a subcommand is left for clap to reject with the real list.
///
/// Inserting (rather than reparsing just `["pick"]`) keeps global options,
/// and skipping option values keeps `--repo list` a value, not a command.
fn with_default_command(mut args: Vec<OsString>) -> Vec<OsString> {
    let mut tokens = args.iter().map(|t| t.to_str());
    while let Some(token) = tokens.next() {
        match token {
            Some(t) if VALUE_OPTIONS.contains(&t) => {
                tokens.next();
            }
            Some(t) if t.starts_with('-') && !INFO_FLAGS.contains(&t) => {}
            _ => return args,
        }
    }
    args.insert(0, "pick".into());
    args
}

fn finite_seconds(value: &str) -> Result<f64, String> {
    match value.trim().parse::<f64>() {
        Ok(v) if v.is_finite() && v >= 0.0 => Ok(v),
        _ => Err(format!(
            "must be a non-negative finite number of seconds, got '{value}'"
        )),
    }
}

fn directory(value: &str) -> Result<PathBuf, String> {
    std::fs::canonicalize(expand_tilde(Path::new(value)))
        .ok()
        .filter(|p| p.is_dir())
        .ok_or_else(|| format!("not a directory: '{value}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        super::parse(args.iter().map(OsString::from).collect())
    }

    fn pick(args: &[&str]) -> (f64, ViewArgs) {
        match parse(args).unwrap() {
            Cli {
                max_age,
                command: Command::Pick(v),
            } => (max_age, v),
            other => panic!("not pick: {other:?}"),
        }
    }

    #[test]
    fn default_command_keeps_global_options() {
        for args in [
            &["--dormant", "--max-age", "0"][..],
            &["--max-age=0", "--dormant"],
            &["--max-age", "0", "pick", "--dormant"],
            &["pick", "--dormant", "--max-age", "0"],
        ] {
            let (max_age, view) = pick(args);
            assert_eq!(max_age, 0.0, "{args:?}");
            assert!(view.dormant, "{args:?}");
        }
        assert_eq!(pick(&[]).0, 30.0);
    }

    #[test]
    fn repo_value_is_not_a_command() {
        assert_eq!(pick(&["--repo", "list"]).1.repo.as_deref(), Some("list"));
        assert_eq!(pick(&["--project", "x"]).1.repo.as_deref(), Some("x"));
    }

    #[test]
    fn help_is_root_help() {
        for args in [&["--help"][..], &["--max-age", "0", "--help"]] {
            let err = parse(args).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
            assert!(err.to_string().contains("Refresh the cache"), "{args:?}");
        }
    }

    #[test]
    fn help_version_and_errors_name_the_real_command() {
        let help = parse(&["--help"]).unwrap_err();
        assert!(help.to_string().contains("Usage: mnemoloom mux"), "{help}");
        let version = parse(&["--version"]).unwrap_err();
        assert_eq!(version.kind(), clap::error::ErrorKind::DisplayVersion);
        let bogus = parse(&["bogus"]).unwrap_err().to_string();
        assert!(bogus.contains("unrecognized subcommand"), "{bogus}");
        assert!(!bogus.contains("mux pick"), "{bogus}");
    }

    #[test]
    fn rejects_bad_numbers_and_args() {
        for args in [
            &["--max-age", "nan"][..],
            &["--max-age", "inf"],
            &["--max-age", "-1"],
            &["list", "--dormant-hours", "nan"],
            &["bogus"],
            &["list", "--dir", "/definitely/not/here"],
            &["list", "--sort", "newest"],
        ] {
            let err = parse(args).unwrap_err();
            assert!(err.use_stderr(), "{args:?}");
        }
    }

    #[test]
    fn subcommand_flags() {
        let cli = parse(&[
            "list",
            "--json",
            "--cached",
            "--sort",
            "oldest",
            "--max-age",
            "5",
        ])
        .unwrap();
        assert_eq!(cli.max_age, 5.0);
        let Command::List(list) = cli.command else {
            panic!("not list")
        };
        assert!(list.json && list.cached && !list.no_color);
        assert_eq!(list.view.sort, SortKey::Oldest);
        assert_eq!(list.view.dormant_hours, 72.0);
        assert_eq!(pick(&["--dir", "/"]).1.dir, Some(PathBuf::from("/")));
        assert!(parse(&["rows"]).is_err());
    }
}
