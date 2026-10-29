//! Action-menu actions, executed in-process through the owning modules
//! (`session::launch`, `commands::files`, `crate::ops`, `crate::menu`).
//! Interactive ones run with the terminal restored to cooked mode.

use super::app::Action;
use crate::cli::{
    CloneArgs, CopyArgs, DeleteArgs, ExportArgs, GlobalArgs, MenuArgs, QueryArgs, RolloverArgs,
    StartScreen,
};
use crate::index::query::Row;
use std::path::Path;

use crate::session::launch::{self, ClaudeFlag, LaunchOptions, prompt_line};
use crate::session::{Agent, Homes, resume_target};
use crate::{cli, clipboard, commands, menu, ops};

/// What the event loop should do after an action.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub status: Option<String>,
    /// Session files may have changed: refresh the index.
    pub reindex: bool,
}

/// How an action uses the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Stays inside the TUI.
    Inline,
    /// Needs the normal terminal; pause afterwards so output can be read.
    Suspended,
    /// Needs the normal terminal; return immediately unless it failed.
    SuspendedQuiet,
}

pub fn kind(a: Action) -> Kind {
    match a {
        Action::View | Action::Path | Action::CopyId => Kind::Inline,
        Action::Delete => Kind::SuspendedQuiet,
        _ => Kind::Suspended,
    }
}

/// Whether `a` rewrites, copies or deletes the session file.
pub fn rewrites(a: Action) -> bool {
    matches!(
        a,
        Action::Clone
            | Action::Copy
            | Action::Trim
            | Action::SmartTrim
            | Action::Rollover
            | Action::Delete
    )
}

/// Status shown instead of running `a` on a session whose agent can't be
/// rewritten ([`Agent::is_rewritable`]); `None` when `a` may run.
pub fn refusal(a: Action, row: &Row) -> Option<String> {
    (rewrites(a) && !row.agent.is_rewritable()).then(|| {
        format!(
            "{} is not supported for {} sessions yet",
            label_word(a),
            row.agent.label()
        )
    })
}

fn agent_arg(a: Agent) -> Option<cli::AgentArg> {
    Some(match a {
        Agent::Claude => cli::AgentArg::Claude,
        Agent::Codex => cli::AgentArg::Codex,
        Agent::Pi => cli::AgentArg::Pi,
        Agent::OpenCode => cli::AgentArg::OpenCode,
    })
}

/// Run a non-inline action (terminal already restored by the caller).
pub fn run_external(a: Action, row: &Row, global: &GlobalArgs, homes: &Homes) -> Outcome {
    let session = Some(row.file_path.clone());
    let agent = agent_arg(row.agent);
    let result: anyhow::Result<()> = match a {
        Action::Resume => launch::exec_resume(
            row.agent,
            &resume_target(row.agent, Path::new(&row.file_path)),
            Some(&row.cwd),
            homes,
            LaunchOptions {
                claude_flag: ClaudeFlag::Short,
                confirm_chdir: true,
            },
        ),
        Action::Clone => ops::clone::clone(global, &CloneArgs { session, agent }).map(drop),
        Action::Copy => commands::files::copy(
            global,
            &CopyArgs {
                session,
                agent,
                ..Default::default()
            },
        )
        .map(drop),
        Action::Export => ops::export::export(
            global,
            &ExportArgs {
                session,
                agent,
                ..Default::default()
            },
        )
        .map(drop),
        Action::Query => ops::query::query(
            global,
            &QueryArgs {
                session,
                agent,
                ..Default::default()
            },
        )
        .map(drop),
        Action::Trim | Action::SmartTrim => menu::menu(
            global,
            &MenuArgs {
                session,
                agent,
                start_screen: if a == Action::Trim {
                    StartScreen::TrimMenu
                } else {
                    StartScreen::SmartTrimForm
                },
                ..Default::default()
            },
        )
        .map(drop),
        Action::Rollover => ops::rollover::rollover(
            global,
            &RolloverArgs {
                session,
                agent,
                ..Default::default()
            },
        )
        .map(drop),
        Action::Delete => commands::files::delete(
            global,
            &DeleteArgs {
                session: row.file_path.clone(),
                agent,
                force: true,
            },
        )
        .map(drop),
        Action::View | Action::Path | Action::CopyId => Ok(()),
    };
    let reindex = !matches!(a, Action::Export | Action::Query);
    let failed = result.is_err();
    if let Err(e) = result {
        eprintln!("Error: {e:#}");
    }
    if failed || kind(a) == Kind::Suspended {
        // Best effort: a closed stdin just returns immediately.
        let _ = prompt_line("\nPress Enter to return to search...");
    }
    let status = match (a, failed) {
        (Action::Delete, false) => Some(format!("Deleted session {}", row.session_id)),
        (_, true) => Some(format!("{} failed", label_word(a))),
        _ => None,
    };
    Outcome { status, reindex }
}

fn label_word(a: Action) -> &'static str {
    match a {
        Action::Resume => "Resume",
        Action::Clone => "Clone",
        Action::Copy => "Copy",
        Action::Export => "Export",
        Action::Query => "Query",
        Action::Trim => "Trim",
        Action::SmartTrim => "Smart trim",
        Action::Rollover => "Rollover",
        Action::Delete => "Delete",
        Action::View => "View",
        Action::Path => "Path",
        Action::CopyId => "Copy ID",
    }
}

/// Status text for the inline `copy_id` / `path` actions.
pub fn inline_status(a: Action, row: &Row) -> Option<String> {
    match a {
        Action::CopyId => Some(if clipboard::copy(&row.session_id) {
            format!("Copied: {}", row.session_id)
        } else {
            "Clipboard not available".to_string()
        }),
        Action::Path => Some(format!("Path: {}", row.file_path)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind(Action::View), Kind::Inline);
        assert_eq!(kind(Action::CopyId), Kind::Inline);
        assert_eq!(kind(Action::Resume), Kind::Suspended);
        assert_eq!(kind(Action::Delete), Kind::SuspendedQuiet);
    }

    #[test]
    fn rewriting_actions_are_refused_for_pi_and_opencode() {
        let row = |agent| crate::index::tui::facets::tests::row(0, agent, "p");
        for a in Action::ALL {
            for agent in [Agent::Claude, Agent::Codex] {
                assert_eq!(refusal(a, &row(agent)), None, "{a:?} {agent:?}");
            }
            for agent in [Agent::Pi, Agent::OpenCode] {
                assert_eq!(refusal(a, &row(agent)).is_some(), rewrites(a), "{a:?}");
            }
        }
        assert_eq!(
            refusal(Action::Trim, &row(Agent::OpenCode)).unwrap(),
            "Trim is not supported for opencode sessions yet"
        );
        assert_eq!(refusal(Action::Resume, &row(Agent::Pi)), None);
        assert_eq!(refusal(Action::Export, &row(Agent::Pi)), None);
    }
}
