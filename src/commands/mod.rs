//! Commands implemented directly over `crate::session`: resolve, info,
//! lineage, find-original, find-derived, resume (plain), copy, move,
//! move-account, delete.

pub mod files;
pub mod info;
pub mod lineage;
pub mod move_account;
pub mod resolve;
pub mod resume;

use std::io::IsTerminal;
use std::path::Path;

use crate::cli::GlobalArgs;
use crate::exit::Exit;
use crate::session::discover::{Latest, current_git_branch, latest_for_cwd};
use crate::session::resolve::{QueryError, SessionRecord, choose_latest, resolve_query};
use crate::session::{Agent, Homes, Resolution};

/// The latest session for the current project/branch. With
/// one candidate per agent, a TTY gets the picker (`prompt`); otherwise the
/// newest wins. Nothing found -> exit 1 with an error message.
pub fn latest_session(
    homes: &Homes,
    agent: Option<Agent>,
    prompt: &str,
) -> anyhow::Result<Resolution> {
    latest_session_with(homes, agent, prompt, true)
}

/// [`latest_session`]; `interactive = false` (e.g. `--json`) never prompts
/// and takes the newest candidate.
pub fn latest_session_with(
    homes: &Homes,
    agent: Option<Agent>,
    prompt: &str,
    interactive: bool,
) -> anyhow::Result<Resolution> {
    let cwd = std::env::current_dir()?;
    let branch = current_git_branch(&cwd);
    let mut found = latest_for_cwd(homes, agent, &cwd, branch.as_deref());
    let desc = match agent {
        Some(a) => a.label(),
        None => "Claude/Codex",
    };
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
    found.sort_by_key(|l| std::cmp::Reverse(l.session.modified));
    let idx = if found.len() > 1 && interactive && std::io::stdin().is_terminal() {
        let fallback = if found.iter().all(|l| l.branch_matched) {
            ""
        } else {
            " (fallback to any branch if none found)"
        };
        let scope = format!("Latest {desc} sessions for '{project}'{on_branch}{fallback}");
        let records: Vec<SessionRecord> = found
            .iter()
            .map(|l| SessionRecord::from_session(&l.session, homes.home(l.session.agent)))
            .collect();
        match choose_latest(&scope, &records, prompt)? {
            Some(i) => i,
            None => return Err(Exit::fail("Session selection cancelled.").into()),
        }
    } else {
        0
    };
    let Latest { session, .. } = found.swap_remove(idx);
    Ok(Resolution {
        agent: session.agent,
        session_file: session.path,
        cwd: session.cwd,
        git_branch: session.git_branch,
    })
}

/// `export-claude <codex id>` (and vice versa): when `query` was not found
/// for `agent` but exists for the other agent, say so instead of
/// `Session not found`. Any other error is returned unchanged.
pub fn wrong_agent_export(
    e: anyhow::Error,
    global: &GlobalArgs,
    query: Option<&str>,
    agent: Agent,
) -> anyhow::Error {
    let not_found = e
        .downcast_ref::<Exit>()
        .and_then(|x| x.message.as_deref())
        .is_some_and(|m| m.starts_with("Error: Session not found"));
    let other = agent.other();
    match (query, global.homes()) {
        (Some(q), Ok(homes))
            if not_found
                && !matches!(
                    resolve_query(q, Some(other), &homes),
                    Err(QueryError::Failed(_))
                ) =>
        {
            Exit::fail(format!(
                "Error: {q} is a {} session; use export-{other} or export",
                other.label()
            ))
            .into()
        }
        _ => e,
    }
}

/// File name of a path as a string.
pub fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Python `f"{n:,}"`, for signed and unsigned counts.
pub fn thousands(n: impl Into<i128>) -> String {
    let n = n.into();
    let s = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    #[test]
    fn export_names_the_other_agent() {
        let d = tempfile::tempdir().unwrap();
        let (claude, codex) = (d.path().join("claude"), d.path().join("codex"));
        write_codex(
            &codex,
            CODEX_ID,
            "2026-05-07T12-57-30",
            &codex_lines(CODEX_ID, "/w", "cli"),
        );
        std::fs::create_dir_all(claude.join("projects")).unwrap();
        let global = GlobalArgs {
            claude_home: Some(claude),
            codex_home: Some(codex),
            pi_home: Some(d.path().join("pi")),
            opencode_home: Some(d.path().join("opencode")),
        };
        let not_found = || anyhow::Error::from(Exit::fail("Error: Session not found: x"));
        let msg = |e: anyhow::Error| e.downcast::<Exit>().unwrap().message.unwrap();
        assert_eq!(
            msg(wrong_agent_export(
                not_found(),
                &global,
                Some(CODEX_ID),
                Agent::Claude
            )),
            format!("Error: {CODEX_ID} is a Codex session; use export-codex or export")
        );
        assert_eq!(
            msg(wrong_agent_export(
                not_found(),
                &global,
                Some("nope"),
                Agent::Claude
            )),
            "Error: Session not found: x"
        );
    }

    #[test]
    fn rewriting_commands_refuse_pi_and_opencode() {
        use crate::cli::*;
        let d = tempfile::tempdir().unwrap();
        let (pi, oc) = (d.path().join("pi"), d.path().join("opencode"));
        let pi_file = write_pi(&pi, PI_ID, "/w", &pi_lines(PI_ID, "/w"));
        write_opencode_db(&oc.join("opencode.db"), "/w", 1_000);
        let global = GlobalArgs {
            claude_home: Some(d.path().join("claude")),
            codex_home: Some(d.path().join("codex")),
            pi_home: Some(pi),
            opencode_home: Some(oc),
        };
        let msg = |r: anyhow::Result<std::process::ExitCode>| {
            r.unwrap_err().downcast::<Exit>().unwrap().message.unwrap()
        };
        for (id, label) in [(PI_ID, "pi"), (OPENCODE_ID, "opencode")] {
            let refused =
                |cmd: &str| format!("Error: \"{cmd}\" is not supported for {label} sessions yet.");
            let session = Some(id.to_string());
            let agent = None;
            assert_eq!(
                msg(files::copy(
                    &global,
                    &CopyArgs {
                        session: session.clone(),
                        dest: Some("/nope".into()),
                        agent
                    }
                )),
                refused("copy")
            );
            let mv = MoveArgs {
                session: id.into(),
                new_project: d.path().into(),
                agent,
            };
            assert_eq!(msg(files::move_session(&global, &mv)), refused("move"));
            let del = DeleteArgs {
                session: id.into(),
                agent,
                force: true,
            };
            assert_eq!(msg(files::delete(&global, &del)), refused("delete"));
            assert_eq!(
                msg(crate::ops::clone::clone(
                    &global,
                    &CloneArgs {
                        session: session.clone(),
                        agent
                    }
                )),
                refused("clone")
            );
            assert_eq!(
                msg(crate::ops::port::port(
                    &global,
                    &PortArgs { session: id.into() }
                )),
                refused("port")
            );
            let ro = RolloverArgs {
                session: session.clone(),
                quick: true,
                ..Default::default()
            };
            assert_eq!(
                msg(crate::ops::rollover::rollover(&global, &ro)),
                refused("rollover")
            );
            let trim = TrimArgs {
                session: session.clone(),
                tools: Some("bash".into()),
                threshold: 500,
                trim_assistant: None,
                output_dir: None,
                agent,
                simple_ui: true,
            };
            assert_eq!(msg(crate::ops::trim::trim(&global, &trim)), refused("trim"));
        }
        let tip = TrimInPlaceArgs {
            session: pi_file.display().to_string(),
            tools: None,
            threshold: 500,
            trim_assistant: None,
            dry_run: true,
            json: false,
        };
        assert_eq!(
            format!(
                "{:#}",
                crate::ops::trim::trim_in_place(&global, &tip).unwrap_err()
            ),
            "Error: \"trim-in-place\" is not supported for pi sessions yet."
        );
        assert!(
            std::fs::read_to_string(&pi_file)
                .unwrap()
                .contains("rate limiter")
        );
    }

    #[test]
    fn thousands() {
        assert_eq!(super::thousands(0), "0");
        assert_eq!(super::thousands(999), "999");
        assert_eq!(super::thousands(1234567), "1,234,567");
        assert_eq!(super::thousands(-1234567), "-1,234,567");
        assert_eq!(super::thousands(i64::MIN), "-9,223,372,036,854,775,808");
        assert_eq!(super::thousands(u64::MAX), "18,446,744,073,709,551,615");
    }
}
