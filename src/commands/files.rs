//! `mnemoloom copy`, `mnemoloom move`, `mnemoloom delete`.

use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use serde_json::Value;

use super::latest_session;
use crate::cli::{CopyArgs, DeleteArgs, GlobalArgs, MoveArgs, agent_of};
use crate::exit::Exit;
use crate::session::encode::project_dir;
use crate::session::homes::expand_tilde;
use crate::session::launch::{
    ClaudeFlag, LaunchOptions, exec_resume, prompt_from, prompt_line, resume_hint,
};
use crate::session::timefmt::display_in_own_offset;
use crate::session::transcript::{WriteStyle, temp_sibling, write_atomic_with};
use crate::session::{Agent, ResolveOptions, Transcript, path_session_uuid, resolve};

/// Python `shutil.copy2`: contents + permissions + mtime/atime.
///
/// Refuses when `dest` is the same file as `src` (same dev+ino, so also
/// hardlinks and symlinks to it), like `shutil.SameFileError`. Copies into
/// a temp file next to `dest` and renames it into place, so a failed copy
/// never truncates an existing `dest`.
pub fn copy2(src: &Path, dest: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let md = fs::metadata(src)?;
    if let Ok(d) = fs::metadata(dest)
        && (d.dev(), d.ino()) == (md.dev(), md.ino())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "'{}' and '{}' are the same file",
                src.display(),
                dest.display()
            ),
        ));
    }
    let tmp = temp_sibling(dest);
    let result = (|| {
        fs::copy(src, &tmp)?;
        let times = fs::FileTimes::new()
            .set_modified(md.modified()?)
            .set_accessed(md.accessed()?);
        fs::File::options()
            .write(true)
            .open(&tmp)?
            .set_times(times)?;
        fs::rename(&tmp, dest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn yes(answer: Option<String>) -> bool {
    matches!(
        answer.as_deref().map(str::to_lowercase).as_deref(),
        Some("y" | "yes")
    )
}

/// `mnemoloom copy [SESSION] [-d DEST] [--agent]`.
pub fn copy(global: &GlobalArgs, args: &CopyArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    let agent = agent_of(args.agent);
    let res = match &args.session {
        Some(q) => resolve(q, &ResolveOptions::new(&homes).agent(agent))?,
        None => latest_session(&homes, agent, "Which session do you want to use?")?,
    };
    res.agent.require_rewritable("copy")?;
    copy_session_file(&res.session_file, args.dest.as_deref())
}

/// Copy a session file: prompts for a destination when none given;
/// directories receive the original file name; missing dirs are offered
/// for creation.
pub fn copy_session_file(source: &Path, dest_override: Option<&str>) -> anyhow::Result<ExitCode> {
    let dest = match dest_override {
        Some(d) => d.to_string(),
        None => match prompt_line("\nEnter destination file or directory path: ")? {
            Some(d) if !d.is_empty() => d,
            _ => {
                println!("Cancelled.");
                return Ok(ExitCode::SUCCESS);
            }
        },
    };
    let dest_path = expand_tilde(Path::new(&dest));
    let name = source.file_name().unwrap_or_default();
    let dest_file = if dest_path.exists() {
        if dest_path.is_dir() {
            dest_path.join(name)
        } else {
            dest_path
        }
    } else if dest.ends_with('/') {
        if !yes(prompt_line(&format!(
            "Directory {} does not exist. Create it? [y/N]: ",
            dest_path.display()
        ))?) {
            println!("Cancelled.");
            return Ok(ExitCode::SUCCESS);
        }
        fs::create_dir_all(&dest_path)?;
        dest_path.join(name)
    } else {
        if let Some(parent) = dest_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty() && !p.exists())
        {
            if !yes(prompt_line(&format!(
                "Parent directory {} does not exist. Create it? [y/N]: ",
                parent.display()
            ))?) {
                println!("Cancelled.");
                return Ok(ExitCode::SUCCESS);
            }
            fs::create_dir_all(parent)?;
        }
        dest_path
    };
    if let Err(e) = copy2(source, &dest_file) {
        println!("\nError copying file: {e}");
        return Ok(ExitCode::from(1));
    }
    println!("\nCopied to: {}", dest_file.display());
    Ok(ExitCode::SUCCESS)
}

/// Rewrite every `cwd` field to `new_cwd`; returns (old cwd, first
/// non-blank Claude `sessionId`). Untouched lines stay byte-identical.
pub fn rewrite_cwd(t: &mut Transcript, new_cwd: &str) -> (Option<String>, Option<String>) {
    let mut old = None;
    let mut sid = None;
    let agent = t.agent;
    for line in &mut t.lines {
        let Some(v) = line.value().filter(|v| v.is_object()) else {
            continue;
        };
        match agent {
            Agent::Claude => {
                if sid.is_none()
                    && let Some(s) = v
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                {
                    sid = Some(s.trim().to_string());
                }
                if let Some(c) = v.get("cwd") {
                    if old.is_none() {
                        old = Some(c.as_str().map_or_else(|| c.to_string(), str::to_string));
                    }
                    line.value_mut().expect("parsed")["cwd"] = Value::String(new_cwd.to_string());
                }
            }
            Agent::Codex => {
                let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
                let Some(c) = v
                    .get("payload")
                    .filter(|p| p.is_object())
                    .and_then(|p| p.get("cwd"))
                else {
                    continue;
                };
                match ty {
                    "session_meta" | "turn_context" => {
                        if old.is_none() {
                            old = Some(c.as_str().map_or_else(|| c.to_string(), str::to_string));
                        }
                    }
                    "response_item" => {}
                    _ => continue,
                }
                line.value_mut().expect("parsed")["payload"]["cwd"] =
                    Value::String(new_cwd.to_string());
            }
            // Never moved (see `Agent::require_rewritable`).
            Agent::Pi | Agent::OpenCode => {}
        }
    }
    (old, sid)
}

/// `mnemoloom move SESSION NEW_PROJECT [--agent]`.
pub fn move_session(global: &GlobalArgs, args: &MoveArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    let target = expand_tilde(&args.new_project);
    let new_project =
        fs::canonicalize(&target).unwrap_or_else(|_| crate::session::absolute(&target));
    if !new_project.exists() {
        return Err(Exit::fail(format!(
            "Error: Project directory does not exist: {}",
            new_project.display()
        ))
        .into());
    }
    if !new_project.is_dir() {
        return Err(
            Exit::fail(format!("Error: Not a directory: {}", new_project.display())).into(),
        );
    }
    let res = resolve(
        &args.session,
        &ResolveOptions::new(&homes)
            .agent(agent_of(args.agent))
            .unique_file(true),
    )?;
    res.agent.require_rewritable("move")?;
    let src = res.session_file.clone();
    println!("Reading session: {}", src.display());
    let new_cwd = new_project.to_string_lossy().into_owned();
    let (mut t, snap) = Transcript::load_with_snapshot(&src, res.agent)?;
    let unchanged = || snap.ensure_unchanged(&src);
    let (old_cwd, content_sid) = rewrite_cwd(&mut t, &new_cwd);

    let session_id = match res.agent {
        Agent::Claude => {
            let sid = match content_sid {
                Some(s) => uuid::Uuid::parse_str(&s)
                    .map(|u| u.hyphenated().to_string())
                    .map_err(|_| {
                        Exit::fail(format!(
                            "Error: Invalid sessionId in Claude transcript: {s}"
                        ))
                    })?,
                None => src
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            };
            let dir = project_dir(&homes.claude, &new_cwd);
            fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            let dest = dir.join(format!("{sid}.jsonl"));
            if dest.exists() && !same_file(&dest, &src) {
                return Err(Exit::fail(format!(
                    "Error: Session already exists at destination: {}",
                    dest.display()
                ))
                .into());
            }
            println!("Moving session to: {}", dest.display());
            let in_place = same_file(&dest, &src) || dest == src;
            let bytes = t.to_bytes(WriteStyle::Compact);
            write_atomic_with(&dest, &bytes, Some(&src), None, &unchanged)?;
            if !in_place {
                // Re-check right before dropping the source: anything appended
                // after the read would otherwise be lost.
                if let Err(e) = unchanged() {
                    let _ = fs::remove_file(&dest);
                    return Err(e);
                }
                fs::remove_file(&src)?;
                println!("Removed old file: {}", src.display());
            }
            println!("\nSession moved successfully!");
            sid
        }
        Agent::Codex => {
            println!("Updating session in place: {}", src.display());
            write_atomic_with(
                &src,
                &t.to_bytes(WriteStyle::Compact),
                None,
                None,
                &unchanged,
            )?;
            println!("\nSession updated successfully!");
            path_session_uuid(&src)
        }
        Agent::Pi | Agent::OpenCode => unreachable!("refused by require_rewritable"),
    };
    println!("  From: {}", old_cwd.as_deref().unwrap_or("None"));
    println!("  To:   {new_cwd}");
    println!();
    if confirm_switch(&mut std::io::stdin().lock(), &new_cwd)? {
        println!("\nSwitching to {new_cwd}...");
        let opts = LaunchOptions {
            claude_flag: ClaudeFlag::Long,
            confirm_chdir: false,
        };
        exec_resume(res.agent, &session_id, Some(&new_cwd), &homes, opts)?;
    } else {
        println!("\nTo resume later:");
        println!(
            "  {}",
            resume_hint(
                res.agent,
                &session_id,
                Some(&new_cwd),
                ClaudeFlag::Long,
                &homes
            )
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `Switch to <cwd> and resume session? [y/n] (y)`: empty means yes, EOF no.
fn confirm_switch(input: &mut impl BufRead, new_cwd: &str) -> std::io::Result<bool> {
    let answer = prompt_from(
        input,
        &format!("Switch to {new_cwd} and resume session? [y/n] (y): "),
    )?;
    Ok(answer.is_some_and(|a| {
        a.is_empty() || a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes")
    }))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Lines, first/last timestamp and last user message (delete preview).
struct DeleteInfo {
    total_lines: usize,
    first_ts: Option<String>,
    last_ts: Option<String>,
    last_user: Option<String>,
}

fn delete_info(path: &Path) -> anyhow::Result<DeleteInfo> {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(fs::File::open(path)?);
    let mut info = DeleteInfo {
        total_lines: 0,
        first_ts: None,
        last_ts: None,
        last_user: None,
    };
    let mut buf = Vec::new();
    while reader.read_until(b'\n', &mut buf)? > 0 {
        info.total_lines += 1;
        if let Ok(v) = serde_json::from_slice::<Value>(buf.trim_ascii()) {
            note_delete_line(&mut info, &v);
        }
        buf.clear();
    }
    Ok(info)
}

fn note_delete_line(info: &mut DeleteInfo, v: &Value) {
    let ts = if v.get("timestamp").is_some() {
        v.get("timestamp")
    } else {
        v.get("created_at")
    };
    if let Some(ts) = ts.filter(|t| !t.is_null() && t.as_str() != Some("")) {
        let s = ts.as_str().map_or_else(|| ts.to_string(), str::to_string);
        info.first_ts.get_or_insert_with(|| s.clone());
        info.last_ts = Some(s);
    }
    let (content, block_type) = match v.get("type").and_then(Value::as_str) {
        Some("user") => (v.pointer("/message/content"), "text"),
        Some("response_item")
            if v.pointer("/payload/type").and_then(Value::as_str) == Some("message")
                && v.pointer("/payload/role").and_then(Value::as_str) == Some("user") =>
        {
            (v.pointer("/payload/content"), "input_text")
        }
        _ => return,
    };
    match content {
        Some(Value::String(s)) if block_type == "text" => {
            info.last_user = Some(s.trim().to_string())
        }
        Some(Value::Array(items)) => {
            for i in items {
                if i.get("type").and_then(Value::as_str) == Some(block_type)
                    && let Some(t) = i
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                {
                    info.last_user = Some(t.to_string());
                }
            }
        }
        _ => {}
    }
}

/// `mnemoloom delete SESSION [--agent] [-f]`. Deletes only the transcript.
pub fn delete(global: &GlobalArgs, args: &DeleteArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    let res = resolve(
        &args.session,
        &ResolveOptions::new(&homes)
            .agent(agent_of(args.agent))
            .prompt("Which session do you want to delete?")
            .unique_file(true),
    )?;
    res.agent.require_rewritable("delete")?;
    let file: PathBuf = res.session_file;
    if !args.force {
        let info = delete_info(&file)
            .map_err(|e| Exit::fail(format!("Error reading session file: {e}")))?;
        let rule = "=".repeat(70);
        println!("\n{rule}");
        println!("SESSION DELETION CONFIRMATION");
        println!("{rule}");
        println!("\nFile: {}", file.display());
        println!("Session ID: {}", path_session_uuid(&file));
        println!("\nTotal lines: {}", info.total_lines);
        let first = info
            .first_ts
            .as_deref()
            .map_or("Unknown".into(), display_in_own_offset);
        let last = info
            .last_ts
            .as_deref()
            .map_or("Unknown".into(), display_in_own_offset);
        if first != "Unknown" || last != "Unknown" {
            println!("Date range: {first} → {last}");
        }
        if let Some(msg) = &info.last_user {
            let shown = if msg.chars().count() > 200 {
                format!("{}...", msg.chars().take(200).collect::<String>())
            } else {
                msg.clone()
            };
            println!("\nLast user message:");
            println!("  {shown}");
        }
        println!("\n{rule}");
        if !yes(prompt_line(
            "\nAre you sure you want to DELETE this session? (yes/no): ",
        )?) {
            println!("\nDeletion cancelled.");
            return Ok(ExitCode::SUCCESS);
        }
    }
    if let Err(e) = fs::remove_file(&file) {
        bail!("\n❌ Error deleting session: {e}");
    }
    println!("\n✅ Session deleted: {}", path_session_uuid(&file));
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    #[test]
    fn move_resume_prompt_eof_is_no() {
        assert!(!confirm_switch(&mut "".as_bytes(), "/p").unwrap());
        assert!(!confirm_switch(&mut "n\n".as_bytes(), "/p").unwrap());
        assert!(confirm_switch(&mut "\n".as_bytes(), "/p").unwrap());
        assert!(confirm_switch(&mut "Yes\n".as_bytes(), "/p").unwrap());
    }

    #[test]
    fn rewrite_cwd_claude_and_codex() {
        let d = tempfile::tempdir().unwrap();
        let p = write_claude(
            d.path(),
            CLAUDE_ID,
            "/old",
            &claude_lines(CLAUDE_ID, "/old"),
        );
        let mut t = Transcript::load(&p, Agent::Claude).unwrap();
        let (old, sid) = rewrite_cwd(&mut t, "/new");
        assert_eq!(
            (old.as_deref(), sid.as_deref()),
            (Some("/old"), Some(CLAUDE_ID))
        );
        let out = String::from_utf8(t.to_bytes(WriteStyle::Compact)).unwrap();
        assert!(!out.contains("\"/old\"") && out.contains("\"cwd\":\"/new\""));
        // Lines without cwd are untouched.
        assert_eq!(
            out.lines().next().unwrap(),
            claude_lines(CLAUDE_ID, "/old")[0]
        );

        let x = write_codex(
            d.path(),
            CODEX_ID,
            "2026-01-02T12-57-30",
            &codex_lines(CODEX_ID, "/old", "cli"),
        );
        let mut t = Transcript::load(&x, Agent::Codex).unwrap();
        let (old, _) = rewrite_cwd(&mut t, "/new");
        assert_eq!(old.as_deref(), Some("/old"));
        let out = String::from_utf8(t.to_bytes(WriteStyle::Compact)).unwrap();
        assert_eq!(out.matches("\"cwd\":\"/new\"").count(), 2);
    }

    #[test]
    fn delete_preview() {
        let d = tempfile::tempdir().unwrap();
        let p = write_claude(d.path(), CLAUDE_ID, "/w", &claude_lines(CLAUDE_ID, "/w"));
        let i = delete_info(&p).unwrap();
        assert_eq!(i.total_lines, 10);
        assert_eq!(i.first_ts.as_deref(), Some("2026-01-03T03:28:25.917Z"));
        assert_eq!(i.last_ts.as_deref(), Some("2026-01-03T03:30:00.000Z"));
        assert_eq!(
            i.last_user.as_deref(),
            Some("fix the failing spec in user_spec.rb")
        );
        let x = write_codex(
            d.path(),
            CODEX_ID,
            "2026-01-02T12-57-30",
            &codex_lines(CODEX_ID, "/w", "cli"),
        );
        assert_eq!(
            delete_info(&x).unwrap().last_user.as_deref(),
            Some("hello codex")
        );
    }

    #[test]
    fn copy2_refuses_same_file_and_never_truncates() {
        // Regression: copying onto the source used to empty it.
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.jsonl");
        fs::write(&src, "data\n").unwrap();
        let err = copy2(&src, &src).unwrap_err();
        assert!(err.to_string().contains("are the same file"));
        let link = d.path().join("link.jsonl");
        std::os::unix::fs::symlink(&src, &link).unwrap();
        assert!(copy2(&src, &link).is_err());
        let hard = d.path().join("hard.jsonl");
        fs::hard_link(&src, &hard).unwrap();
        assert!(copy2(&src, &hard).is_err());
        assert_eq!(fs::read(&src).unwrap(), b"data\n");
        // copy_session_file into the source's own dir reports an error, exit 1.
        let code = copy_session_file(&src, Some(d.path().to_str().unwrap())).unwrap();
        assert_eq!(code, ExitCode::from(1));
        assert_eq!(fs::read(&src).unwrap(), b"data\n");
        // A failed copy leaves an existing destination intact.
        let dest = d.path().join("dest.jsonl");
        fs::write(&dest, "keep").unwrap();
        assert!(copy2(&d.path().join("missing"), &dest).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"keep");
    }

    #[test]
    fn move_aborts_when_session_grows() {
        // Regression: appends after the read must not be lost.
        let d = tempfile::tempdir().unwrap();
        let src = write_claude(
            d.path(),
            CLAUDE_ID,
            "/old",
            &claude_lines(CLAUDE_ID, "/old"),
        );
        let (mut t, snap) = Transcript::load_with_snapshot(&src, Agent::Claude).unwrap();
        rewrite_cwd(&mut t, "/new");
        let mut f = fs::OpenOptions::new().append(true).open(&src).unwrap();
        std::io::Write::write_all(&mut f, b"{\"type\":\"user\"}\n").unwrap();
        let err = write_atomic_with(&src, &t.to_bytes(WriteStyle::Compact), None, None, &|| {
            snap.ensure_unchanged(&src)
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("changed while"));
        assert!(
            fs::read_to_string(&src)
                .unwrap()
                .ends_with("{\"type\":\"user\"}\n")
        );
    }

    #[test]
    fn copy2_preserves_mtime() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.jsonl");
        fs::write(&src, "x").unwrap();
        set_mtime(&src, 12_345);
        let dest = d.path().join("b.jsonl");
        copy2(&src, &dest).unwrap();
        assert_eq!(
            fs::metadata(&dest).unwrap().modified().unwrap(),
            fs::metadata(&src).unwrap().modified().unwrap()
        );
    }
}
