//! `mnemoloom mux`: see and jump between coding agents running in tmux panes.
//!
//! `mnemoloom mux` opens the picker, `mnemoloom mux list [--json]` prints
//! every live agent, `mnemoloom mux scan` refreshes the cache (for cron or
//! tmux hooks).

mod activity;
mod cache;
mod cli;
mod detect;
mod exec;
mod filters;
mod git;
mod model;
mod paths;
mod picker;
mod procs;
mod render;
mod scan;
mod tmux;

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use mnemoloom::session::timefmt::now;

use self::cli::{Command, ListArgs, ViewArgs};
use self::filters::{View, display};
use self::model::Agent;

/// Entry point. `args` are the mux args after `mnemoloom mux`.
pub fn main(args: Vec<OsString>) -> ExitCode {
    let cli = match cli::parse(args) {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return if err.use_stderr() {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    if !tmux::server_running() {
        if tmux::installed() {
            eprintln!("mnemoloom mux: no tmux server running");
        } else {
            eprintln!("mnemoloom mux: tmux not found on PATH");
        }
        return ExitCode::FAILURE;
    }
    match cli.command {
        Command::Pick(view) => cmd_pick(view, cli.max_age),
        Command::List(list) => cmd_list(list, cli.max_age),
        Command::Scan => cmd_scan(),
    }
}

/// Scan live panes and rewrite the cache (which always holds every agent).
fn fresh() -> Vec<Agent> {
    let now = now();
    let agents = scan::scan(now);
    cache::write(&agents, now);
    agents
}

/// Cached agents when fresh enough, else a fresh scan; plus whether cached.
fn agents_for_display(max_age: f64) -> (Vec<Agent>, bool) {
    let (cached, _) = cache::read(Some(max_age), now());
    if cached.is_empty() {
        (fresh(), false)
    } else {
        (cached, true)
    }
}

/// Print to stdout, tolerating a closed pipe (`mnemoloom mux list | head`).
fn emit(text: &str) {
    let _ = writeln!(std::io::stdout().lock(), "{text}");
}

fn cmd_list(args: ListArgs, max_age: f64) -> ExitCode {
    let agents = if args.cached {
        agents_for_display(max_age).0
    } else {
        fresh()
    };
    let agents = display(agents, &View::from(args.view), now());
    if args.json {
        match serde_json::to_string_pretty(&agents) {
            Ok(text) => emit(&text),
            Err(err) => {
                eprintln!("mnemoloom mux: {err}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        let colour = std::io::stdout().is_terminal() && !args.no_color;
        emit(&render::table(&agents, colour));
    }
    ExitCode::SUCCESS
}

fn cmd_scan() -> ExitCode {
    let agents = fresh();
    emit(&format!(
        "cached {} agents -> {}",
        agents.len(),
        cache::path().display()
    ));
    ExitCode::SUCCESS
}

fn cmd_pick(args: ViewArgs, max_age: f64) -> ExitCode {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("mnemoloom mux: no tty -- use 'mnemoloom mux list' for scripted output");
        return ExitCode::FAILURE;
    }
    let view = View::from(args);
    let (agents, mut from_cache) = agents_for_display(max_age);
    let mut agents = display(agents, &view, now());
    // Cached rows the filters exclude may be stale; confirm with a real scan.
    if agents.is_empty() && from_cache {
        agents = display(fresh(), &view, now());
        from_cache = false;
    }
    if agents.is_empty() {
        emit("no matching agents");
        return ExitCode::SUCCESS;
    }
    let pane = match picker::pick(agents, &view, from_cache) {
        Ok(Some(pane)) => pane,
        Ok(None) => return ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("mnemoloom mux: {err:#}");
            return ExitCode::FAILURE;
        }
    };
    match tmux::switch_to(&pane) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("mnemoloom mux: tmux attach: {err}");
            ExitCode::FAILURE
        }
    }
}
