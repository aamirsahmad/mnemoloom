//! Session action menu: `mnemoloom`, `mnemoloom <session>`,
//! `mnemoloom menu`, and the screens other commands open when given no flags.
//!
//! `state` is a pure reducer over [`state::Screen`]s, `ui` renders it,
//! `tui` drives the terminal, `simple` is the `--simple-ui` / non-TTY text
//! menu, and `dispatch` runs the chosen action via `commands` / `ops`.

mod dispatch;
mod follow_up;
mod forms;
mod session;
mod simple;
mod state;
mod tui;
mod ui;

use std::io::{self, IsTerminal};
use std::process::ExitCode;

pub use self::follow_up::after_trim;
use self::session::{PreviewKind, SessionView};
use self::simple::Prompter;
use self::state::{Action, DirInfo, Screen, State};
pub use self::state::{DirChoice, TrimChoice, TrimInfo};
use crate::cli::{GlobalArgs, MenuArgs, StartScreen, agent_of};
use crate::exit::Exit;
use crate::session::discover::{current_git_branch, latest_for_cwd};
use crate::session::{Agent, Homes, ResolveOptions, resolve};

/// `mnemoloom menu [SESSION] [--agent] [--start-screen S] [--simple-ui] [--shell]`.
/// `session == None` = latest session(s) for the current project.
pub fn menu(global: &GlobalArgs, args: &MenuArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    let agent = agent_of(args.agent);
    let (mut sessions, scope) = match &args.session {
        Some(q) => {
            let opts = ResolveOptions::new(&homes)
                .agent(agent)
                .prompt("Which session do you want to open?");
            let res = resolve(q, &opts)?;
            (vec![SessionView::from_resolution(&res)?], None)
        }
        None => latest(&homes, agent)?,
    };
    let tty = io::stdin().is_terminal() && io::stdout().is_terminal();
    let picked = if args.simple_ui || !tty {
        let mut p = Prompter {
            input: io::stdin().lock(),
            out: io::stdout(),
        };
        p.run(&sessions, scope.as_deref())?
    } else {
        let state = State::new(args.start_screen, sessions.len());
        let title = results_title(args.start_screen);
        tui::run(state, &mut sessions, &homes, title, scope.as_deref())?
    };
    match picked {
        Some((i, action)) => dispatch::run(global, args, &sessions[i], action),
        None => Ok(ExitCode::SUCCESS),
    }
}

/// Results-picker title per start screen.
fn results_title(start: StartScreen) -> &'static str {
    match start {
        StartScreen::Action => " Select a session ",
        StartScreen::Resume => " Which session to resume? ",
        StartScreen::TrimMenu => " Which session to trim? ",
        StartScreen::SmartTrimForm => " Which session to smart-trim? ",
        StartScreen::Lineage => " Which session to rollover? ",
        StartScreen::Query => " Select session for query ",
    }
}

/// Latest-session candidates (one per agent) plus the scope line.
fn latest(
    homes: &Homes,
    agent: Option<Agent>,
) -> anyhow::Result<(Vec<SessionView>, Option<String>)> {
    let cwd = std::env::current_dir()?;
    let branch = current_git_branch(&cwd);
    let found = latest_for_cwd(homes, agent, &cwd, branch.as_deref());
    let desc = agent.map_or("Claude/Codex", Agent::label);
    let project = cwd.file_name().unwrap_or_default().to_string_lossy();
    let on_branch = branch
        .as_deref()
        .map(|b| format!(" on branch '{b}'"))
        .unwrap_or_default();
    if found.is_empty() {
        return Err(Exit::fail(format!(
            "No {desc} sessions found for '{project}'{on_branch} (even with fallback to any branch)."
        ))
        .into());
    }
    let fallback = if branch.is_some() {
        " (fallback to any branch if none found)"
    } else {
        ""
    };
    let scope = format!("Latest {desc} sessions for '{project}'{on_branch}{fallback}");
    let sessions = found
        .iter()
        .map(|l| SessionView::from_file(&l.session, PreviewKind::LastMessage))
        .collect();
    Ok((sessions, Some(scope)))
}

/// Trim-confirm dialog: shown after a trim/smart-trim created
/// (or declined to create) a new session. Non-TTY → `Cancel`.
pub fn trim_confirm(info: TrimInfo) -> anyhow::Result<TrimChoice> {
    if !io::stdout().is_terminal() {
        return Ok(TrimChoice::Cancel);
    }
    Ok(match tui::run_dialog(Screen::TrimConfirm(info))? {
        Some(Action::TrimConfirm(c)) => c,
        _ => TrimChoice::Cancel,
    })
}

/// Directory-confirm dialog: the session lives in another directory.
/// Non-TTY → `Cancel`.
pub fn dir_confirm(current_dir: &str, session_dir: &str) -> anyhow::Result<DirChoice> {
    if !io::stdout().is_terminal() {
        return Ok(DirChoice::Cancel);
    }
    let info = DirInfo {
        current_dir: current_dir.to_string(),
        session_dir: session_dir.to_string(),
    };
    Ok(match tui::run_dialog(Screen::DirConfirm(info))? {
        Some(Action::DirConfirm(c)) => c,
        _ => DirChoice::Cancel,
    })
}
