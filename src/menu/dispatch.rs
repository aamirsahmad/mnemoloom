//! Run the action chosen in the menu by calling the command/op that owns
//! it. The terminal has already been restored when this runs.

use std::path::PathBuf;
use std::process::ExitCode;

use super::forms::{RolloverOptions, RolloverType};
use super::session::SessionView;
use super::state::Action;
use crate::cli::{
    AgentArg, CloneArgs, CopyArgs, ExportArgs, GlobalArgs, MenuArgs, QueryArgs, ResumeArgs,
    RolloverArgs, SmartTrimArgs, TrimArgs,
};
use crate::session::homes::expand_tilde;
use crate::session::launch::{ClaudeFlag, command_hint, resume_argv, shell_quote};
use crate::session::{Agent, absolute};
use crate::{commands, ops};

/// Custom context-recovery prompt; `None` for quick rollover or for the
/// default instructions (sent as `--context`).
fn rollover_prompt(o: &RolloverOptions) -> Option<String> {
    match o.rollover_type {
        RolloverType::Context if !o.prompt.trim().is_empty() => Some(o.prompt.clone()),
        _ => None,
    }
}

fn agent_arg(agent: Agent) -> AgentArg {
    match agent {
        Agent::Claude => AgentArg::Claude,
        Agent::Codex => AgentArg::Codex,
        Agent::Pi => AgentArg::Pi,
        Agent::OpenCode => AgentArg::OpenCode,
    }
}

/// Export always writes a `.txt` file: force the suffix.
fn txt_path(dest: PathBuf) -> PathBuf {
    let dest = expand_tilde(&dest);
    if dest.extension().is_some_and(|e| e == "txt") {
        dest
    } else {
        dest.with_extension("txt")
    }
}

/// Legacy `--shell` resume: print the commands instead of running them.
fn print_shell_resume(global: &GlobalArgs, s: &SessionView) -> anyhow::Result<ExitCode> {
    if let Some(cwd) = s.cwd.as_deref().filter(|c| !c.is_empty()) {
        println!("cd {}", shell_quote(cwd));
    }
    match s.agent {
        Agent::Claude => {
            let homes = global.homes()?;
            let env = if homes.claude_overridden {
                let home = absolute(&homes.claude);
                format!(
                    "CLAUDE_CONFIG_DIR={} ",
                    shell_quote(&home.to_string_lossy())
                )
            } else {
                String::new()
            };
            println!("{env}claude -r {}", s.session_id);
        }
        Agent::Codex => println!("codex resume {}", s.session_id),
        agent @ (Agent::Pi | Agent::OpenCode) => {
            let homes = global.homes()?;
            let target = crate::session::resume_target(agent, &s.path);
            let argv = resume_argv(agent, &target, ClaudeFlag::Short);
            println!("{}", command_hint(agent, &argv, None, &homes));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Execute `action` on `s`.
pub fn run(
    global: &GlobalArgs,
    args: &MenuArgs,
    s: &SessionView,
    action: Action,
) -> anyhow::Result<ExitCode> {
    let session = Some(s.path.display().to_string());
    let agent = Some(agent_arg(s.agent));
    match action {
        Action::Path => {
            println!("\nSession file path:\n{}", s.path.display());
            Ok(ExitCode::SUCCESS)
        }
        Action::Copy { dest } => commands::files::copy(
            global,
            &CopyArgs {
                session,
                dest: Some(dest),
                agent,
            },
        ),
        Action::Export { dest } => ops::export::export(
            global,
            &ExportArgs {
                session,
                agent,
                output: Some(txt_path(dest)),
                verbose: false,
            },
        ),
        Action::Query { question } => ops::query::query(
            global,
            &QueryArgs {
                session,
                question: Some(question),
                agent,
            },
        ),
        Action::Resume if args.shell => print_shell_resume(global, s),
        Action::Resume => commands::resume::run(global, &ResumeArgs { session, agent }),
        Action::Clone => ops::clone::clone(global, &CloneArgs { session, agent }),
        Action::Trim(o) => ops::trim::trim(
            global,
            &TrimArgs {
                session,
                tools: o.tools,
                threshold: o.threshold,
                trim_assistant: o.trim_assistant,
                output_dir: None,
                agent,
                // Direct mode: never re-open this menu.
                simple_ui: true,
            },
        ),
        Action::SmartTrim { instructions } => ops::trim::smart_trim(
            global,
            &SmartTrimArgs {
                session,
                instructions: Some(instructions),
                exclude_types: None,
                preserve_recent: 10,
                content_threshold: 200,
                output_dir: None,
                dry_run: false,
                agent,
            },
        ),
        Action::Rollover(o) => ops::rollover::rollover(
            global,
            &RolloverArgs {
                session,
                quick: o.rollover_type == RolloverType::Quick,
                prompt: rollover_prompt(&o),
                context: o.rollover_type == RolloverType::Context,
                target_agent: Some(agent_arg(o.agent)),
                agent,
            },
        ),
        // Dialog results are returned to the dialog's caller, never dispatched.
        Action::TrimConfirm(_) | Action::DirConfirm(_) => Ok(ExitCode::SUCCESS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollover_prompts() {
        let mut o = RolloverOptions {
            agent: Agent::Claude,
            rollover_type: RolloverType::Quick,
            prompt: String::new(),
        };
        assert_eq!(rollover_prompt(&o), None);
        o.rollover_type = RolloverType::Context;
        assert_eq!(rollover_prompt(&o), None);
        o.prompt = "focus".into();
        assert_eq!(rollover_prompt(&o).as_deref(), Some("focus"));
    }

    #[test]
    fn export_suffix() {
        assert_eq!(txt_path("/a/b.md".into()), PathBuf::from("/a/b.txt"));
        assert_eq!(txt_path("/a/b".into()), PathBuf::from("/a/b.txt"));
        assert_eq!(txt_path("/a/b.txt".into()), PathBuf::from("/a/b.txt"));
    }
}
