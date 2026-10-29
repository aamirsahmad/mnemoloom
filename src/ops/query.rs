//! `query`: export the session to a temp file,
//! then ask the session's own agent about that file headlessly
//! (menu RPC form: Claude `-p --no-session-persistence --model haiku`,
//! Codex `exec --json --model gpt-5.1-codex-mini`) and print the answer.
//! Any helper session the agent persisted is marked and deleted.
//!
//! The CLI and the menu share this path. The export is written to a private
//! (0700, random name) `$TMPDIR/mnemoloom-query-*` directory (never the
//! session's project dir) and deleted once the agent is done.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;

use super::export::render_markdown;
use super::headless::{
    AgentBins, HelperSession, ScratchDir, capture, interrupted_exit, json_lines, last_json_object,
};
use crate::cli::{GlobalArgs, MenuArgs, QueryArgs, StartScreen, agent_of};
use crate::exit::Exit;
use crate::session::{Agent, Homes, Resolution, ResolveOptions, Transcript, resolve};

const TIMEOUT: Duration = Duration::from_secs(300);
const CLAUDE_MODEL: &str = "haiku";
const CODEX_MODEL: &str = "gpt-5.1-codex-mini";

/// `mnemoloom query [SESSION] [QUESTION] [--agent]`. Without a question the
/// menu's query screen opens.
pub fn query(global: &GlobalArgs, args: &QueryArgs) -> anyhow::Result<ExitCode> {
    let question = args.question.as_deref().filter(|q| !q.is_empty());
    let (Some(q), Some(question)) = (&args.session, question) else {
        return crate::menu::menu(
            global,
            &MenuArgs {
                session: args.session.clone(),
                agent: args.agent,
                start_screen: StartScreen::Query,
                ..Default::default()
            },
        );
    };
    let homes = global.homes()?;
    let res = resolve(q, &ResolveOptions::new(&homes).agent(agent_of(args.agent)))?;
    let answer = export_and_ask(&res, question, &homes, &AgentBins::from_env())?;
    println!("\n{answer}");
    Ok(ExitCode::SUCCESS)
}

/// Export `res`, ask its agent about it, then delete the export.
fn export_and_ask(
    res: &Resolution,
    question: &str,
    homes: &Homes,
    bins: &AgentBins,
) -> anyhow::Result<String> {
    let (_scratch, export) = export_session(res)?;
    let agent = query_agent(res.agent);
    println!("\n🔍 Querying session with {}...", agent.label());
    ask(agent, &export, question, homes, bins)
}

/// The agent that answers: the session's own for Claude / Codex; Claude
/// for pi / opencode sessions (their export is plain text either way).
fn query_agent(session: Agent) -> Agent {
    match session {
        Agent::Codex => Agent::Codex,
        Agent::Claude | Agent::Pi | Agent::OpenCode => Agent::Claude,
    }
}

/// Write the transcript export to `<private scratch dir>/<session id>.txt`
/// (outside the project). The file lives as long as the returned dir.
fn export_session(res: &Resolution) -> anyhow::Result<(ScratchDir, PathBuf)> {
    let t = Transcript::load(&res.session_file, res.agent)?;
    let text = render_markdown(&t)?;
    let scratch = ScratchDir::new("mnemoloom-query-").context("creating the query temp dir")?;
    println!("\n📄 Exporting session...");
    let name = format!("{}.txt", res.session_id());
    let path = scratch
        .write(&name, text.as_bytes())
        .with_context(|| format!("writing {}", scratch.path().join(&name).display()))?;
    println!("\n📄 Exported to: {}", path.display());
    Ok((scratch, path))
}

/// The menu's query prompt for `agent`.
fn prompt(agent: Agent, export_path: &Path, query: &str) -> String {
    let p = export_path.display();
    match agent {
        Agent::Claude | Agent::Pi | Agent::OpenCode => format!(
            "There is a log of a past conversation with an AI agent in this file: {p}\n\n\
             Strategically use PARALLEL SUB-AGENTS to explore {p} (which may be very long) to answer the following question:\n\n\
             {query}\n\n\
             DO NOT TRY TO READ {p} by YOURSELF! To save your own context, you must use parallel sub-agents, possibly to explore the beginning, middle, and end of that chat.\n\n\
             Provide a clear and concise answer to the question."
        ),
        Agent::Codex => format!(
            "There is a log of a past conversation with an AI agent in this file: {p}\n\n\
             CAUTION: {p} may be very large. Strategically use parallel sub-agents if available, or use another strategy to efficiently read the file so your context window is not overloaded. For example, you could read specific sections (beginning, middle, end) rather than the entire file at once.\n\n\
             Based on the session log, answer the following question:\n\n\
             {query}\n\n\
             Provide a clear and concise answer."
        ),
    }
}

/// Headless query argv (without the program).
fn query_args(agent: Agent, prompt: String) -> Vec<String> {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match agent {
        Agent::Claude | Agent::Pi | Agent::OpenCode => {
            let mut args = v(&["-p", "--no-session-persistence"]);
            args.push(prompt);
            args.extend(v(&[
                "--model",
                CLAUDE_MODEL,
                "--permission-mode",
                "bypassPermissions",
                "--output-format",
                "json",
            ]));
            args
        }
        Agent::Codex => {
            let mut args = v(&["exec", "--json", "--model", CODEX_MODEL]);
            args.push(prompt);
            args
        }
    }
}

/// Answer and helper-session id from the agent's stdout: Claude `.result`
/// / `.session_id` of the last JSON object; Codex the last
/// `item.completed` agent message / `thread.started.thread_id`.
fn parse_answer(agent: Agent, stdout: &str) -> (Option<String>, Option<String>) {
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);
    match agent {
        Agent::Claude | Agent::Pi | Agent::OpenCode => last_json_object(stdout)
            .map_or((None, None), |v| {
                (text(v.get("result")), text(v.get("session_id")))
            }),
        Agent::Codex => {
            let events: Vec<Value> = json_lines(stdout).collect();
            let of_type = |t: &'static str| {
                events
                    .iter()
                    .filter(move |e| e.get("type").and_then(Value::as_str) == Some(t))
            };
            let answer = of_type("item.completed")
                .rfind(|e| e.pointer("/item/type").and_then(Value::as_str) == Some("agent_message"))
                .and_then(|e| text(e.pointer("/item/text")));
            let thread = of_type("thread.started")
                .next()
                .and_then(|e| text(e.get("thread_id")));
            (answer, thread)
        }
    }
}

/// Ask `agent` about the export (headless, stdin closed, 300 s timeout).
fn ask(
    agent: Agent,
    export_path: &Path,
    question: &str,
    homes: &Homes,
    bins: &AgentBins,
) -> anyhow::Result<String> {
    let args = query_args(agent, prompt(agent, export_path, question));
    let out =
        capture(bins, agent, &args, Some(homes), Some(TIMEOUT)).map_err(|e| match e.kind() {
            ErrorKind::Interrupted => interrupted_exit(),
            ErrorKind::TimedOut => Exit::fail("Error: Query timed out after 300 seconds"),
            _ => Exit::fail(format!("Error: running {agent}: {e}")),
        })?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (answer, helper) = parse_answer(agent, &stdout);
    let _helper = HelperSession {
        agent,
        id: helper,
        homes,
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Exit::fail(format!(
            "Error: {agent} query failed: {}",
            stderr.trim().chars().take(200).collect::<String>()
        ))
        .into());
    }
    answer.ok_or_else(|| Exit::fail(format!("Error: no answer in {agent} output")).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake(dir: &Path, name: &str, body: &str) -> AgentBins {
        let p = dir.join(format!("fake-{name}"));
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        AgentBins {
            claude: p.clone().into(),
            codex: p.into(),
        }
    }

    fn fixture(dir: &Path, id: &str) -> Resolution {
        let cwd = dir.join("proj");
        let src = write_raw(
            &dir.join("s").join(format!("{id}.jsonl")),
            &claude_lines(id, &cwd.display().to_string()),
        );
        Resolution {
            agent: Agent::Claude,
            session_file: src,
            cwd: Some(cwd.display().to_string()),
            git_branch: None,
        }
    }

    #[test]
    fn opencode_sessions_export_and_ask_claude() {
        let dir = tempfile::tempdir().unwrap();
        let db = write_opencode_db(&dir.path().join("opencode.db"), "/w", 1_000);
        let res = Resolution {
            agent: Agent::OpenCode,
            session_file: crate::session::opencode::virtual_path(&db, OPENCODE_ID),
            cwd: Some("/w".into()),
            git_branch: None,
        };
        let (_scratch, path) = export_session(&res).unwrap();
        assert!(path.ends_with(format!("{OPENCODE_ID}.txt")));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("add a rate limiter")
        );
        assert_eq!(query_agent(Agent::OpenCode), Agent::Claude);
        assert_eq!(query_agent(Agent::Pi), Agent::Claude);
        assert_eq!(query_agent(Agent::Codex), Agent::Codex);
    }

    #[test]
    fn exports_to_temp_not_project() {
        let dir = tempfile::tempdir().unwrap();
        let res = fixture(dir.path(), CLAUDE_ID);
        let (scratch, path) = export_session(&res).unwrap();
        let (again, other) = export_session(&res).unwrap();
        assert_ne!(path, other, "concurrent queries share a path");
        assert_eq!(path.parent(), Some(scratch.path()));
        assert!(path.starts_with(std::env::temp_dir()));
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(scratch.path()), 0o700);
        assert_eq!(mode(&path), 0o600);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("fix the failing spec")
        );
        assert!(!dir.path().join("proj").exists());
        drop((scratch, again));
        assert!(!path.exists() && !path.parent().unwrap().exists());
    }

    #[test]
    fn export_is_removed_after_success_and_failure() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
        let res = fixture(dir.path(), CLAUDE_ID2);
        let seen = dir.path().join("seen");
        let seen_path = dir.path().join("seen-path");
        for (exit, ok) in [(0, true), (2, false)] {
            // The export path is the end of the prompt's first line ($3).
            let bins = fake(
                dir.path(),
                "claude",
                &format!(
                    "p=$(printf '%s\\n' \"$3\" | head -n 1 | sed 's/.*in this file: //')\n\
                     printf '%s' \"$p\" > '{}'\ncp \"$p\" '{}'\n\
                     echo '{{\"result\":\"ok\"}}'\nexit {exit}",
                    seen_path.display(),
                    seen.display()
                ),
            );
            let result = export_and_ask(&res, "q", &homes, &bins);
            assert_eq!(result.is_ok(), ok);
            assert!(
                std::fs::read_to_string(&seen)
                    .unwrap()
                    .contains("fix the failing spec")
            );
            let path = PathBuf::from(std::fs::read_to_string(&seen_path).unwrap());
            assert!(!path.exists() && !path.parent().unwrap().exists());
            std::fs::remove_file(&seen).unwrap();
        }
    }

    #[test]
    fn claude_query_is_not_persisted_and_helper_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
        let args_file = dir.path().join("args");
        // A persisted helper (should --no-session-persistence be ignored).
        let helper = write_claude(
            &homes.claude,
            CLAUDE_ID2,
            "/x",
            &claude_lines(CLAUDE_ID2, "/x"),
        );
        let bins = fake(
            dir.path(),
            "claude",
            &format!(
                "printf '%s\\n' \"$@\" > '{}'\necho noise\necho '{{\"type\":\"result\",\"result\":\"It was the auth bug.\",\"session_id\":\"{CLAUDE_ID2}\"}}'",
                args_file.display()
            ),
        );
        let export = dir.path().join("e.txt");
        let answer = ask(Agent::Claude, &export, "what broke?", &homes, &bins).unwrap();
        assert_eq!(answer, "It was the auth bug.");
        let p = prompt(Agent::Claude, &export, "what broke?");
        assert_eq!(
            std::fs::read_to_string(&args_file).unwrap(),
            format!(
                "-p\n--no-session-persistence\n{p}\n--model\nhaiku\n--permission-mode\nbypassPermissions\n--output-format\njson\n"
            )
        );
        assert!(p.contains("Strategically use PARALLEL SUB-AGENTS to explore"));
        assert!(!helper.exists());
    }

    #[test]
    fn codex_session_is_queried_with_codex() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
        let args_file = dir.path().join("args");
        let helper = write_codex(
            &homes.codex,
            CODEX_ID2,
            "2026-05-07T12-57-30",
            &codex_lines(CODEX_ID2, "/w", "exec"),
        );
        let bins = fake(
            dir.path(),
            "codex",
            &format!(
                "printf '%s\\n' \"$@\" > '{}'\n\
                 echo '{{\"type\":\"thread.started\",\"thread_id\":\"{CODEX_ID2}\"}}'\n\
                 echo '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"draft\"}}}}'\n\
                 echo '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"final\"}}}}'",
                args_file.display()
            ),
        );
        let export = dir.path().join("e.txt");
        let answer = ask(Agent::Codex, &export, "q?", &homes, &bins).unwrap();
        assert_eq!(answer, "final");
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(args.starts_with("exec\n--json\n--model\ngpt-5.1-codex-mini\nThere is a log"));
        assert!(args.contains("CAUTION:"));
        assert!(!helper.exists());
    }

    #[test]
    fn failed_query_reports_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
        let helper = write_claude(
            &homes.claude,
            CLAUDE_ID2,
            "/x",
            &claude_lines(CLAUDE_ID2, "/x"),
        );
        let bins = fake(
            dir.path(),
            "claude",
            &format!("echo '{{\"session_id\":\"{CLAUDE_ID2}\"}}'\necho denied >&2\nexit 2"),
        );
        let err = ask(Agent::Claude, Path::new("/e.txt"), "q", &homes, &bins).unwrap_err();
        assert_eq!(err.to_string(), "Error: claude query failed: denied");
        assert!(!helper.exists());
    }
}
