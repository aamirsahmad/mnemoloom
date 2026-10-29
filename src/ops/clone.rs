//! `clone`: copy a session under a new uuid4 and
//! resume the copy. No lineage metadata is added.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use regex::Regex;
use serde_json::{Value, json};

use crate::cli::{CloneArgs, GlobalArgs, agent_of};
use crate::commands::files::copy2;
use crate::commands::latest_session;
use crate::exit::Exit;
use crate::session::encode::project_dir;
use crate::session::launch::{LaunchOptions, exec_resume};
use crate::session::transcript::WriteStyle;
use crate::session::{Agent, Homes, ResolveOptions, Transcript, resolve};

/// `mnemoloom clone [SESSION] [--agent]`. Without SESSION the latest session of
/// the current project is cloned directly.
pub fn clone(global: &GlobalArgs, args: &CloneArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    let agent = agent_of(args.agent);
    let res = match &args.session {
        Some(q) => resolve(q, &ResolveOptions::new(&homes).agent(agent))?,
        None => latest_session(&homes, agent, "Which session to clone?")?,
    };
    res.agent.require_rewritable("clone")?;
    let old_id = res.session_id();
    let cwd = res
        .cwd
        .clone()
        .unwrap_or_else(|| parent_dir(&res.session_file));
    let new_id = uuid::Uuid::new_v4().to_string();
    clone_session(res.agent, &res.session_file, &cwd, &new_id, &homes)?;
    println!("\nCloned session:");
    println!("  Original: {old_id}");
    println!("  New:      {new_id}");
    println!("\nResuming cloned session...");
    exec_resume(
        res.agent,
        &new_id,
        Some(&cwd),
        &homes,
        LaunchOptions {
            confirm_chdir: true,
            ..Default::default()
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

fn parent_dir(p: &Path) -> String {
    p.parent().unwrap_or(Path::new("")).display().to_string()
}

/// Copy `source` to the clone path for `new_id` and rewrite its session id.
/// Claude: `<home>/projects/<encode(cwd)>/<new>.jsonl`, every `sessionId`.
/// Codex: same dir and timestamp prefix, `session_meta.payload.id`.
pub fn clone_session(
    agent: Agent,
    source: &Path,
    cwd: &str,
    new_id: &str,
    homes: &Homes,
) -> anyhow::Result<PathBuf> {
    agent.require_rewritable("clone")?;
    if !source.is_file() {
        return Err(Exit::fail(format!(
            "Error: Session file not found: {}",
            source.display()
        ))
        .into());
    }
    let dest = match agent {
        Agent::Claude => {
            let dir = project_dir(&homes.claude, cwd);
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            dir.join(format!("{new_id}.jsonl"))
        }
        Agent::Codex => codex_clone_path(source, new_id)?,
        Agent::Pi | Agent::OpenCode => unreachable!("refused by require_rewritable"),
    };
    copy2(source, &dest).with_context(|| format!("copying to {}", dest.display()))?;
    let mut t = Transcript::load(&dest, agent)?;
    if rewrite_session_ids(&mut t, new_id) {
        t.write_atomic(&dest, WriteStyle::Python, None)?;
    }
    Ok(dest)
}

/// `rollout-<ts>-<old>.jsonl` -> `rollout-<ts>-<new>.jsonl` (same dir).
fn codex_clone_path(source: &Path, new_id: &str) -> anyhow::Result<PathBuf> {
    let name = source.file_name().unwrap_or_default().to_string_lossy();
    let re = Regex::new(r"^(rollout-\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-)(.+)(\.jsonl)$")?;
    let Some(caps) = re.captures(&name) else {
        return Err(Exit::fail(format!(
            "Error: Invalid Codex session filename format: {name}"
        ))
        .into());
    };
    Ok(source.with_file_name(format!("{}{new_id}{}", &caps[1], &caps[3])))
}

/// Claude: every line with a `sessionId` key; Codex: `session_meta.payload.id`.
pub(crate) fn rewrite_session_ids(t: &mut Transcript, new_id: &str) -> bool {
    let agent = t.agent;
    let mut changed = false;
    for line in &mut t.lines {
        let Some(Value::Object(m)) = line.value() else {
            continue;
        };
        let target = match agent {
            Agent::Claude => m.contains_key("sessionId").then_some("/sessionId"),
            Agent::Codex => (m.get("type").and_then(Value::as_str) == Some("session_meta")
                && m.get("payload").is_some_and(|p| p.get("id").is_some()))
            .then_some("/payload/id"),
            Agent::Pi | Agent::OpenCode => None,
        };
        let Some(ptr) = target else { continue };
        if let Some(v) = line.value_mut().and_then(|v| v.pointer_mut(ptr)) {
            *v = json!(new_id);
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    #[test]
    fn claude_clone_rewrites_ids_into_project_dir() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
        let mut lines = claude_lines(CLAUDE_ID, "/w/proj");
        lines.push("not json".into());
        let src = write_raw(&dir.path().join("elsewhere/src.jsonl"), &lines);
        let new_id = "11111111-2222-4333-8444-555555555555";
        let dest = clone_session(Agent::Claude, &src, "/w/proj", new_id, &homes).unwrap();
        assert_eq!(
            dest,
            dir.path()
                .join("claude/projects/-w-proj")
                .join(format!("{new_id}.jsonl"))
        );
        let out = std::fs::read_to_string(&dest).unwrap();
        let out: Vec<&str> = out.lines().collect();
        assert_eq!(out.len(), lines.len());
        for (o, i) in out.iter().zip(&lines) {
            match serde_json::from_str::<Value>(i) {
                Ok(v) if v.get("sessionId").is_some() => {
                    let o: Value = serde_json::from_str(o).unwrap();
                    assert_eq!(o["sessionId"], new_id);
                }
                _ => assert_eq!(o, i, "lines without sessionId are byte-identical"),
            }
        }
        assert!(src.is_file(), "source untouched");
        assert!(!out.iter().any(|l| l.contains("trim_metadata")));
    }

    #[test]
    fn codex_clone_keeps_timestamp_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
        let src = write_codex(
            &homes.codex,
            CODEX_ID,
            "2026-01-02T12-57-30",
            &codex_lines(CODEX_ID, "/w", "cli"),
        );
        let new_id = "11111111-2222-4333-8444-555555555555";
        let dest = clone_session(Agent::Codex, &src, "/w", new_id, &homes).unwrap();
        assert_eq!(
            dest,
            src.with_file_name(format!("rollout-2026-01-02T12-57-30-{new_id}.jsonl"))
        );
        let t = Transcript::load(&dest, Agent::Codex).unwrap();
        assert_eq!(t.lines[0].value().unwrap()["payload"]["id"], new_id);
        assert!(!t.lines[1].is_modified());
        let src_text = std::fs::read_to_string(&src).unwrap();
        let dest_text = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(
            src_text.lines().skip(1).collect::<Vec<_>>(),
            dest_text.lines().skip(1).collect::<Vec<_>>()
        );
    }

    #[test]
    fn codex_clone_rejects_bad_name() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
        let src = write_raw(
            &dir.path().join("x.jsonl"),
            &codex_lines(CODEX_ID, "/w", "cli"),
        );
        let err = clone_session(Agent::Codex, &src, "/w", "n", &homes).unwrap_err();
        assert_eq!(
            err.downcast_ref::<Exit>().unwrap().message.as_deref(),
            Some("Error: Invalid Codex session filename format: x.jsonl")
        );
    }
}
