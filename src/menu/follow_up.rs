//! What happens after a trim / smart-trim created (or declined to create) a
//! new session: on a TTY the trim-confirm dialog,
//! otherwise the trim CLI's `Resume this session now? [Y/n]` prompt.
//! Resuming replaces `mnemoloom` (exec); every other dialog choice goes back to
//! the menu's resume screen (the `back` choice).

use std::io::{self, IsTerminal};
use std::process::ExitCode;

use anyhow::Context;

use super::state::{TrimChoice, TrimInfo};
use crate::cli::{AgentArg, GlobalArgs, MenuArgs, StartScreen};
use crate::ops::trim::TrimFollowUp;
use crate::session::launch::{ClaudeFlag, LaunchOptions, exec_resume, prompt_line, resume_hint};
use crate::session::{Agent, Homes};

/// Outcome of the trim-confirm dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Next {
    /// Resume this session id.
    Resume(String),
    /// Delete the new session file, then go back.
    Delete,
    /// Keep the new file (message), then go back.
    Keep,
    /// Re-open the menu at the resume screen.
    Back,
}

fn next(choice: TrimChoice, f: &TrimFollowUp) -> Next {
    match (choice, &f.new) {
        (TrimChoice::Resume, Some((id, _))) => Next::Resume(id.clone()),
        (TrimChoice::Resume, None) => Next::Resume(f.original_id.clone()),
        (TrimChoice::Delete, Some(_)) => Next::Delete,
        (_, Some(_)) => Next::Keep,
        (_, None) => Next::Back,
    }
}

fn info(f: &TrimFollowUp) -> TrimInfo {
    TrimInfo {
        new_session_id: f.new.as_ref().map(|(id, _)| id.clone()),
        original_session_id: Some(f.original_id.clone()),
        lines_trimmed: usize::try_from(f.lines_trimmed).unwrap_or(usize::MAX),
        tokens_saved: usize::try_from(f.tokens_saved).unwrap_or(0),
        output_file: f
            .new
            .as_ref()
            .map(|(_, p)| p.display().to_string())
            .unwrap_or_default(),
        nothing_to_trim: f.new.is_none(),
    }
}

/// Menu arguments for "back": the original session at the resume screen.
fn back_args(f: &TrimFollowUp) -> MenuArgs {
    MenuArgs {
        session: Some(f.original_file.display().to_string()),
        agent: Some(match f.agent {
            Agent::Claude => AgentArg::Claude,
            Agent::Codex => AgentArg::Codex,
            Agent::Pi => AgentArg::Pi,
            Agent::OpenCode => AgentArg::OpenCode,
        }),
        start_screen: StartScreen::Resume,
        ..Default::default()
    }
}

/// Offer to resume after a trim (see module docs).
pub fn after_trim(
    global: &GlobalArgs,
    f: &TrimFollowUp,
    homes: &Homes,
) -> anyhow::Result<ExitCode> {
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        return text_prompt(f, homes);
    }
    match next(super::trim_confirm(info(f))?, f) {
        Next::Resume(id) => return resume(f, &id, homes),
        Next::Delete => {
            if let Some((_, file)) = &f.new {
                std::fs::remove_file(file)
                    .with_context(|| format!("deleting {}", file.display()))?;
                println!(
                    "\n🗑️  Deleted session file: {}",
                    crate::commands::file_name(file)
                );
            }
        }
        Next::Keep => {
            if let Some((id, file)) = &f.new {
                println!(
                    "\n📁 Session file kept: {}\n   Session ID: {id}",
                    file.display()
                );
            }
        }
        Next::Back => {}
    }
    super::menu(global, &back_args(f))
}

/// Non-TTY: `Resume this session now? [Y/n]` (only when a session was made).
fn text_prompt(f: &TrimFollowUp, homes: &Homes) -> anyhow::Result<ExitCode> {
    let Some((id, _)) = &f.new else {
        return Ok(ExitCode::SUCCESS);
    };
    let hint = resume_hint(f.agent, id, f.cwd.as_deref(), ClaudeFlag::Long, homes);
    println!("\n");
    match prompt_line("Resume this session now? [Y/n]: ")? {
        None => println!("\n\nTo resume later, run:\n  {hint}"),
        Some(a) if matches!(a.to_lowercase().as_str(), "n" | "no") => {
            println!("\nTo resume later, run:\n  {hint}")
        }
        Some(_) => {
            println!("\n🚀 Resuming session...\n");
            return resume(f, id, homes);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `claude --resume <id>` / `codex resume <id>` in the session cwd,
/// replacing `mnemoloom`.
fn resume(f: &TrimFollowUp, id: &str, homes: &Homes) -> anyhow::Result<ExitCode> {
    let opts = LaunchOptions {
        claude_flag: ClaudeFlag::Long,
        confirm_chdir: false,
    };
    exec_resume(f.agent, id, f.cwd.as_deref(), homes, opts)?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn follow_up(new: bool) -> TrimFollowUp {
        TrimFollowUp {
            agent: Agent::Codex,
            original_file: PathBuf::from("/h/orig.jsonl"),
            original_id: "orig".into(),
            new: new.then(|| ("new".to_string(), PathBuf::from("/h/new.jsonl"))),
            lines_trimmed: 3,
            tokens_saved: 400,
            cwd: None,
        }
    }

    #[test]
    fn nothing_to_trim_back_reopens_resume_screen() {
        let f = follow_up(false);
        assert_eq!(next(TrimChoice::Back, &f), Next::Back);
        assert_eq!(next(TrimChoice::Resume, &f), Next::Resume("orig".into()));
        let args = back_args(&f);
        assert_eq!(args.start_screen, StartScreen::Resume);
        assert_eq!(args.session.as_deref(), Some("/h/orig.jsonl"));
        assert_eq!(args.agent, Some(AgentArg::Codex));
        assert!(info(&f).nothing_to_trim);
    }

    #[test]
    fn trimmed_choices() {
        let f = follow_up(true);
        assert_eq!(next(TrimChoice::Resume, &f), Next::Resume("new".into()));
        assert_eq!(next(TrimChoice::Delete, &f), Next::Delete);
        assert_eq!(next(TrimChoice::Cancel, &f), Next::Keep);
        let i = info(&f);
        assert_eq!(
            (i.new_session_id.as_deref(), i.output_file.as_str()),
            (Some("new"), "/h/new.jsonl")
        );
    }
}
