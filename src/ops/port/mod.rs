//! Port sessions into new, resumable, flattened transcripts for the other agent.

mod flatten;
mod noise;
mod text;

use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use chrono::{Local, SecondsFormat, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::cli::{GlobalArgs, PortArgs};
use crate::clipboard;
use crate::exit::Exit;
use crate::session::encode::encode_project_path;
use crate::session::launch::{ClaudeFlag, prompt_line, resume_hint};
use crate::session::transcript::{Line, WriteStyle};
use crate::session::{self, Agent, Homes, ResolveOptions, Role, Transcript, pyjson};

use flatten::{FlatMessage, Metadata};

#[derive(Debug)]
struct Ported {
    id: String,
    path: PathBuf,
    cwd: String,
}

/// `mnemoloom port SESSION`.
pub fn port(global: &GlobalArgs, args: &PortArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    print_header(&args.session);
    println!("1/3  Resolving session");
    let resolved = session::resolve(
        &args.session,
        &ResolveOptions::new(&homes).prompt("Which source session do you want to port?"),
    )
    .map_err(|error| {
        let message = error.to_string();
        let detail = message.strip_prefix("Error: ").unwrap_or(&message);
        if detail.starts_with("Session not found:") || args.session.trim().is_empty() {
            Exit::fail(format!(
                "Error: Session not found in Claude or Codex homes: {}",
                args.session
            ))
            .into()
        } else if message == "Session selection cancelled." {
            println!("Port cancelled.");
            anyhow::Error::from(Exit::code(1))
        } else {
            error
        }
    })?;
    let source = resolved.agent;
    source.require_rewritable("port")?;
    let target = source.other();
    let target_label = if target == Agent::Claude {
        "Claude Code"
    } else {
        "Codex"
    };
    println!("  \u{2713} Detected source agent: {source} \u{2014} porting to {target_label}");
    println!("    Source file  {}", resolved.session_file.display());
    println!("2/3  Porting {} \u{2192} {target_label}", source.label());
    let transcript = Transcript::load(&resolved.session_file, source)?;
    let result = convert(&transcript, &homes)?;
    println!("3/3  Ready");
    print_complete(&result, target);
    println!("To resume:");
    println!(
        "  {}",
        resume_hint(
            target,
            &result.id,
            Some(&result.cwd),
            ClaudeFlag::Long,
            &homes
        )
    );
    if target == Agent::Codex {
        println!("\nTip: Codex's /import is also available as an interactive alternative.");
    }
    offer_clipboard(&result.id, target)?;
    Ok(ExitCode::SUCCESS)
}

fn convert(transcript: &Transcript, homes: &Homes) -> anyhow::Result<Ported> {
    let meta = flatten::metadata(transcript);
    let source_id = meta
        .source_id
        .clone()
        .unwrap_or_else(|| session::path_session_uuid(&transcript.path));
    let messages = flatten::ensure_roles(
        flatten::flatten(transcript, &meta),
        transcript.agent,
        &source_id,
    );
    if messages.is_empty() {
        bail!(
            "No portable messages found in {}",
            transcript.path.display()
        );
    }
    match transcript.agent {
        Agent::Claude => write_codex(transcript, homes, &meta, &source_id, &messages),
        Agent::Codex => write_claude(transcript, homes, &meta, &source_id, &messages),
        Agent::Pi | Agent::OpenCode => transcript
            .agent
            .require_rewritable("port")
            .map(|()| unreachable!("refused by require_rewritable")),
    }
}

fn continuation(transcript: &Transcript, source_id: &str, now: &str) -> Value {
    json!({
        "parent_session_id": source_id,
        "parent_session_file": session::absolute(&transcript.path).to_string_lossy(),
        "continued_at": now,
        "ported_from": transcript.agent.as_str()
    })
}

fn publish(path: &Path, agent: Agent, values: Vec<Value>) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .context("output session has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    Transcript {
        path: path.to_path_buf(),
        agent,
        lines: values.into_iter().map(Line::from_value).collect(),
        trailing_newline: true,
    }
    .write_atomic(path, WriteStyle::Python, None)
}

fn fallback_cwd(meta: &Metadata) -> anyhow::Result<String> {
    match meta.cwd.as_ref().filter(|cwd| !cwd.is_empty()) {
        Some(cwd) => Ok(cwd.clone()),
        None => Ok(std::env::current_dir()?.to_string_lossy().into_owned()),
    }
}

fn valid_project(cwd: &str) -> bool {
    !cwd.is_empty() && !cwd.contains('\0') && {
        let component = encode_project_path(cwd);
        // Validate before native long-path hashing, matching the porter's
        // fallback for unusable source project components.
        !component.is_empty()
            && component.len() <= 255
            && session::encode::encode_project_path_legacy(cwd).len() <= 255
    }
}

fn write_claude(
    transcript: &Transcript,
    homes: &Homes,
    meta: &Metadata,
    source_id: &str,
    messages: &[FlatMessage],
) -> anyhow::Result<Ported> {
    let mut cwd = fallback_cwd(meta)?;
    if !valid_project(&cwd) {
        cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    }
    if !valid_project(&cwd) {
        bail!("Cannot derive a usable Claude project directory name from cwd: {cwd:?}");
    }
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false);
    let fallback_ts = meta.timestamp.as_deref().unwrap_or(&now);
    let id = Uuid::new_v4().to_string();
    let path = homes
        .claude_projects()
        .join(encode_project_path(&cwd))
        .join(format!("{id}.jsonl"));
    let mut lines = Vec::with_capacity(messages.len());
    let mut parent: Option<String> = None;
    for message in messages {
        let uuid = Uuid::new_v4().to_string();
        let role = message.role.as_str();
        let mut line = json!({
            "parentUuid": parent,
            "isSidechain": false,
            "userType": "external",
            "cwd": cwd,
            "sessionId": id,
            "version": "2.1.211",
            "gitBranch": meta.branch.as_deref().unwrap_or(""),
            "type": role,
            "message": {"role":role,"content":[{"type":"text","text":message.text}]},
            "uuid": uuid,
            "timestamp": message.timestamp.as_deref().unwrap_or(fallback_ts)
        });
        if lines.is_empty() {
            line["continue_metadata"] = continuation(transcript, source_id, &now);
        }
        lines.push(line);
        parent = Some(uuid);
    }
    publish(&path, Agent::Claude, lines)?;
    Ok(Ported { id, path, cwd })
}

fn write_codex(
    transcript: &Transcript,
    homes: &Homes,
    meta: &Metadata,
    source_id: &str,
    messages: &[FlatMessage],
) -> anyhow::Result<Ported> {
    let cwd = fallback_cwd(meta)?;
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let fallback_ts = meta.timestamp.as_deref().unwrap_or(&now);
    let id = Uuid::now_v7().to_string();
    let local = Local::now();
    let path = homes
        .codex_sessions()
        .join(local.format("%Y/%m/%d").to_string())
        .join(format!(
            "rollout-{}-{id}.jsonl",
            local.format("%Y-%m-%dT%H-%M-%S")
        ));
    let mut payload = json!({
        "session_id": id,
        "id": id,
        "timestamp": now,
        "cwd": cwd,
        "originator": "mnemoloom_port",
        "cli_version": "mnemoloom-port",
        "model_provider": "openai"
    });
    if let Some(branch) = &meta.branch {
        payload["git"] = json!({"branch":branch});
    }
    let mut lines = vec![json!({
        "timestamp": now, "type":"session_meta", "payload":payload,
        "continue_metadata":continuation(transcript, source_id, &now)
    })];
    let mut turn_id = String::new();
    let mut previous_role = None;
    for message in messages {
        if turn_id.is_empty() || message.role == Role::User && previous_role != Some(Role::User) {
            turn_id = Uuid::now_v7().to_string();
        }
        previous_role = Some(message.role);
        let timestamp = message.timestamp.as_deref().unwrap_or(fallback_ts);
        lines.push(codex_item(message, timestamp, &turn_id));
        lines.push(codex_event(message, timestamp));
    }
    publish(&path, Agent::Codex, lines)?;
    let preview = messages
        .iter()
        .find(|m| m.role == Role::User)
        .map(|m| m.text.as_str())
        .unwrap_or("Ported Claude session");
    if let Err(error) = append_history(&homes.codex, &id, preview) {
        fs::remove_file(&path).with_context(|| {
            format!(
                "removing {} after history append failed: {error:#}",
                path.display()
            )
        })?;
        return Err(error);
    }
    Ok(Ported { id, path, cwd })
}

fn codex_item(message: &FlatMessage, timestamp: &str, turn_id: &str) -> Value {
    let user = message.role == Role::User;
    let mut payload = json!({
        "type":"message",
        "role": message.role.as_str(),
        "content":[{"type":if user { "input_text" } else { "output_text" }, "text":message.text}]
    });
    if !user {
        payload["phase"] = json!("final_answer");
    }
    payload["internal_chat_message_metadata_passthrough"] = json!({"turn_id":turn_id});
    json!({"timestamp":timestamp, "type":"response_item", "payload":payload})
}

fn codex_event(message: &FlatMessage, timestamp: &str) -> Value {
    let payload = if message.role == Role::User {
        json!({"type":"user_message", "message":message.text, "images":[], "local_images":[],
            "audio":[], "local_audio":[], "text_elements":[]})
    } else {
        json!({"type":"agent_message", "message":message.text, "phase":"final_answer", "memory_citation":null})
    };
    json!({"timestamp":timestamp, "type":"event_msg", "payload":payload})
}

fn append_history(home: &Path, id: &str, preview: &str) -> anyhow::Result<()> {
    let path = home.join("history.jsonl");
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o644)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.lock()
        .with_context(|| format!("locking {}", path.display()))?;
    let original_size = file.metadata()?.len();
    let mut entry = (pyjson::dumps(&json!({
        "session_id":id, "ts":Utc::now().timestamp(), "text":preview.chars().take(500).collect::<String>()
    })) + "\n").into_bytes();
    let mut last = [0];
    if original_size > 0 && file.read_at(&mut last, original_size - 1)? == 1 && last[0] != b'\n' {
        entry.insert(0, b'\n');
    }
    if let Err(error) = file.write_all(&entry).and_then(|()| file.sync_all()) {
        rollback_history(&file, original_size, &entry);
        return Err(error).with_context(|| format!("appending {}", path.display()));
    }
    Ok(())
}

// flock is advisory: never truncate bytes appended by an uncooperative writer.
fn rollback_history(file: &File, original_size: u64, entry: &[u8]) {
    let Ok(metadata) = file.metadata() else {
        return;
    };
    let Some(length) = metadata.len().checked_sub(original_size) else {
        return;
    };
    if length > entry.len() as u64 {
        return;
    }
    let mut tail = vec![0; length as usize];
    if file.read_exact_at(&mut tail, original_size).is_ok() && tail == entry[..length as usize] {
        let _ = file.set_len(original_size);
    }
}

fn print_header(query: &str) {
    let content = format!("Query  {query}");
    let width = (content.chars().count().max(14) + 4).min(terminal_width());
    panel_border("Session port", width);
    let available = width - 4;
    let mut remaining = content.as_str();
    while !remaining.is_empty() {
        let end = remaining
            .char_indices()
            .nth(available)
            .map_or(remaining.len(), |(index, _)| index);
        let split = if end < remaining.len() {
            remaining[..end]
                .rfind(char::is_whitespace)
                .filter(|index| *index > 0)
                .unwrap_or(end)
        } else {
            end
        };
        panel_row(remaining[..split].trim_end(), width);
        remaining = remaining[split..].trim_start();
    }
    println!("\u{2570}{}\u{256f}", "\u{2500}".repeat(width - 2));
}

fn terminal_width() -> usize {
    crossterm::terminal::size()
        .map(|(width, _)| width as usize)
        .unwrap_or(80)
        .max(40)
}

fn panel_border(title: &str, width: usize) {
    let title = format!(" {title} ");
    let remaining = (width - 2).saturating_sub(title.len());
    let left = remaining / 2;
    println!(
        "\u{256d}{}{title}{}\u{256e}",
        "\u{2500}".repeat(left),
        "\u{2500}".repeat(remaining - left)
    );
}

fn panel_row(content: &str, width: usize) {
    let padding = width.saturating_sub(content.chars().count() + 4);
    println!("\u{2502} {content}{} \u{2502}", " ".repeat(padding));
}

fn print_complete(result: &Ported, target: Agent) {
    let rows = [
        (
            format!("New {} session id:", target.label()),
            result.id.clone(),
        ),
        ("Session cwd:".to_string(), result.cwd.clone()),
        (
            "Output file:".to_string(),
            result.path.display().to_string(),
        ),
    ];
    let label_width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
    let width = terminal_width();
    panel_border("Port complete", width);
    for (label, value) in rows {
        let available = width.saturating_sub(label_width + 6);
        let value = if value.chars().count() > available {
            format!(
                "{}\u{2026}",
                value
                    .chars()
                    .take(available.saturating_sub(1))
                    .collect::<String>()
            )
        } else {
            value
        };
        panel_row(&format!("{label:<label_width$}  {value}"), width);
    }
    println!("\u{2570}{}\u{256f}", "\u{2500}".repeat(width - 2));
    println!("Output file:\n  {}", result.path.display());
}

fn offer_clipboard(id: &str, target: Agent) -> anyhow::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok(());
    }
    let response = prompt_line("\nCopy the new session ID to your clipboard? (Y/n) ")?
        .unwrap_or_else(|| "n".into());
    if !matches!(response.to_ascii_lowercase().as_str(), "" | "y" | "yes") {
        println!("Clipboard left unchanged.");
        return Ok(());
    }
    if clipboard::copy(id) {
        println!("\u{2713} Session ID copied to clipboard.");
        println!("Now you can run:");
        let command = if target == Agent::Claude {
            "claude --resume"
        } else {
            "codex resume"
        };
        println!("  {command} <paste session ID>");
    } else {
        println!(
            "Could not find a working system clipboard command. The session ID is still shown above."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::{
        CLAUDE_ID, CODEX_ID, claude_lines, codex_lines, write_claude, write_codex,
    };

    fn homes() -> (tempfile::TempDir, Homes) {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
        (dir, homes)
    }

    fn fixture(homes: &Homes, agent: Agent, values: &[Value]) -> Transcript {
        let path = match agent {
            Agent::Claude => write_claude(
                &homes.claude,
                CLAUDE_ID,
                "/work",
                &values.iter().map(Value::to_string).collect::<Vec<_>>(),
            ),
            Agent::Codex => write_codex(
                &homes.codex,
                CODEX_ID,
                "2026-05-07T12-57-30",
                &values.iter().map(Value::to_string).collect::<Vec<_>>(),
            ),
            other => unreachable!("{other} is not portable"),
        };
        Transcript::load(&path, agent).unwrap()
    }

    fn values(path: &Path, agent: Agent) -> Vec<Value> {
        Transcript::load(path, agent)
            .unwrap()
            .lines
            .into_iter()
            .filter_map(|line| line.value().cloned())
            .collect()
    }

    #[test]
    fn claude_to_codex_native_shapes_uuid7_turns_and_history() {
        let (_dir, homes) = homes();
        let source = write_claude(
            &homes.claude,
            CLAUDE_ID,
            "/work",
            &claude_lines(CLAUDE_ID, "/work"),
        );
        let transcript = Transcript::load(&source, Agent::Claude).unwrap();
        let result = convert(&transcript, &homes).unwrap();
        assert_eq!(Uuid::parse_str(&result.id).unwrap().get_version_num(), 7);
        let lines = values(&result.path, Agent::Codex);
        assert_eq!(lines.len(), 9);
        assert_eq!(lines[0]["payload"]["session_id"], lines[0]["payload"]["id"]);
        assert_eq!(lines[0]["payload"]["originator"], "mnemoloom_port");
        assert_eq!(lines[0]["payload"]["cli_version"], "mnemoloom-port");
        assert_eq!(lines[0]["payload"]["model_provider"], "openai");
        assert_eq!(
            lines[0]["continue_metadata"]["parent_session_id"],
            CLAUDE_ID
        );
        assert_eq!(lines[0]["continue_metadata"]["ported_from"], "claude");
        assert_eq!(
            lines[1]["payload"]["content"][0]["text"],
            "fix the failing spec in user_spec.rb"
        );
        assert_eq!(
            lines[2]["payload"],
            json!({"type":"user_message","message":"fix the failing spec in user_spec.rb","images":[],"local_images":[],"audio":[],"local_audio":[],"text_elements":[]})
        );
        for pair in lines[1..].chunks_exact(2) {
            assert_eq!(pair[0]["type"], "response_item");
            assert_eq!(pair[1]["type"], "event_msg");
            assert_eq!(pair[0]["timestamp"], pair[1]["timestamp"]);
            assert!(pair[0]["payload"].get("id").is_none());
            assert_eq!(
                pair[0]["payload"]["internal_chat_message_metadata_passthrough"],
                lines[1]["payload"]["internal_chat_message_metadata_passthrough"]
            );
        }
        assert_eq!(lines[3]["payload"]["phase"], "final_answer");
        assert_eq!(lines[4]["payload"]["memory_citation"], Value::Null);
        let history: Value =
            serde_json::from_str(&fs::read_to_string(homes.codex.join("history.jsonl")).unwrap())
                .unwrap();
        assert_eq!(history["session_id"], result.id);
        assert_eq!(history["text"], "fix the failing spec in user_spec.rb");
    }

    #[test]
    fn codex_to_claude_merges_assistant_tools_and_links_uuids() {
        let (_dir, homes) = homes();
        let source = write_codex(
            &homes.codex,
            CODEX_ID,
            "2026-05-07T12-57-30",
            &codex_lines(CODEX_ID, "/work", "cli"),
        );
        let result = convert(&Transcript::load(&source, Agent::Codex).unwrap(), &homes).unwrap();
        assert_eq!(Uuid::parse_str(&result.id).unwrap().get_version_num(), 4);
        assert_eq!(
            result.path.parent().unwrap(),
            homes.claude_projects().join("-work")
        );
        let lines = values(&result.path, Agent::Claude);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["message"]["content"][0]["text"], "hello codex");
        assert_eq!(
            lines[1]["message"]["content"][0]["text"],
            "[codex tool call] exec_command({\"cmd\":\"ls\"})\n\n[codex tool result] a\nb\n\nDone."
        );
        assert_eq!(lines[0]["parentUuid"], Value::Null);
        assert_eq!(lines[1]["parentUuid"], lines[0]["uuid"]);
        assert_eq!(lines[0]["version"], "2.1.211");
        assert_eq!(lines[0]["continue_metadata"]["ported_from"], "codex");
        assert!(lines[1].get("continue_metadata").is_none());
    }

    #[test]
    fn claude_compaction_filters_sidechains_and_keeps_text_untruncated() {
        let (_dir, homes) = homes();
        let transcript = fixture(
            &homes,
            Agent::Claude,
            &[
                json!({"type":"user","message":{"content":"old"}}),
                json!({"type":"user","isCompactSummary":true,"message":{"content":"summary"}}),
                json!({"type":"user","isCompactSummary":true,"isSidechain":true,"message":{"content":"side"}}),
                json!({"type":"user","isCompactSummary":true,"message":{"content":""}}),
                json!({"type":"assistant","message":{"content":[{"type":"text","text":"x".repeat(2000)},{"type":"tool_use","name":"Bash","input":{"command":"y".repeat(2000)}}]}}),
                json!({"type":"user","message":{"content":[{"type":"tool_result","content":"z".repeat(1600)}]}}),
            ],
        );
        let result = convert(&transcript, &homes).unwrap();
        let lines = values(&result.path, Agent::Codex);
        assert_eq!(lines[1]["payload"]["content"][0]["text"], "summary");
        assert_eq!(lines[3]["payload"]["content"][0]["text"], "x".repeat(2000));
        assert!(
            lines[5]["payload"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("... [truncated 515 chars]")
        );
        assert!(
            lines[7]["payload"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .ends_with("... [truncated 100 chars]")
        );
    }

    #[test]
    fn codex_compaction_uses_last_replacement_and_goal_context() {
        let (_dir, homes) = homes();
        let user = |text: &str| json!({"type":"message","role":"user","content":[{"type":"input_text","text":text}]});
        let transcript = fixture(
            &homes,
            Agent::Codex,
            &[
                json!({"type":"response_item","payload":user("old")}),
                json!({"type":"compacted","timestamp":"compact-time","payload":{"replacement_history":[user("kept"),{"type":"compaction","encrypted_content":"secret"},user("<codex_internal_context source=\"goal\">old goal</codex_internal_context>")]}}),
                json!({"type":"response_item","payload":user("<codex_internal_context source=\"goal\">last goal</codex_internal_context>")}),
                json!({"type":"compacted","payload":{"replacement_history":[]}}),
                json!({"type":"response_item","payload":{"type":"agent_message","content":[{"type":"text","text":"routed"}]}}),
                json!({"type":"response_item","payload":{"type":"agent_message","content":[{"type":"text","text":"hidden"},{"type":"encrypted_content","data":"secret"}]}}),
            ],
        );
        let result = convert(&transcript, &homes).unwrap();
        let lines = values(&result.path, Agent::Claude);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0]["message"]["content"][0]["text"],
            "kept\n\n[codex goal context]\n<codex_internal_context source=\"goal\">last goal</codex_internal_context>"
        );
        assert_eq!(lines[1]["message"]["content"][0]["text"], "routed");
    }

    #[test]
    fn synthetic_leading_and_trailing_messages() {
        let (_dir, homes) = homes();
        let transcript = fixture(
            &homes,
            Agent::Codex,
            &[
                json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"first"}]}}),
                json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"last"}]}}),
            ],
        );
        let result = convert(&transcript, &homes).unwrap();
        let lines = values(&result.path, Agent::Claude);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["type"], "user");
        assert!(
            lines[0]["message"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("[Transcript ported from Codex session")
        );
        assert!(
            lines[3]["message"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .ends_with("ended before this user message was answered.]")
        );
    }

    #[test]
    fn authoritative_fork_metadata_and_legacy_cwd() {
        let (_dir, homes) = homes();
        let transcript = fixture(
            &homes,
            Agent::Codex,
            &[
                json!({"type":"session_meta","timestamp":"ancestor-ts","payload":{"id":"ancestor","cwd":"/ancestor","git":{"branch":"old"}}}),
                json!({"type":"session_meta","timestamp":"own-ts","payload":{"id":CODEX_ID,"cwd":"/own"}}),
                json!({"type":"session_meta","payload":{"id":"later","cwd":"/later"}}),
            ],
        );
        let meta = flatten::metadata(&transcript);
        assert_eq!(meta.cwd.as_deref(), Some("/own"));
        assert_eq!(meta.source_id.as_deref(), Some(CODEX_ID));
        assert_eq!(meta.timestamp.as_deref(), Some("own-ts"));
        assert_eq!(meta.branch, None);
        let transcript = fixture(
            &homes,
            Agent::Codex,
            &[
                json!({"id":"legacy","timestamp":"legacy-ts"}),
                json!({"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\nCurrent working directory: /legacy\n</environment_context>"}]}),
                json!({"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}),
            ],
        );
        assert_eq!(
            flatten::metadata(&transcript).cwd.as_deref(),
            Some("/legacy")
        );
        assert!(convert(&transcript, &homes).is_ok());
    }

    #[test]
    fn history_separator_unicode_preview_and_failed_append_cleanup() {
        let (_dir, homes) = homes();
        fs::create_dir_all(&homes.codex).unwrap();
        let history = homes.codex.join("history.jsonl");
        fs::write(&history, "{\"existing\":true}").unwrap();
        append_history(&homes.codex, "new", &"\u{e9}".repeat(700)).unwrap();
        let text = fs::read_to_string(&history).unwrap();
        let entries = text
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1]["text"].as_str().unwrap().chars().count(), 500);
        fs::remove_file(&history).unwrap();
        fs::create_dir(&history).unwrap();
        let transcript = fixture(
            &homes,
            Agent::Claude,
            &[json!({"type":"user","message":{"content":"hello"}})],
        );
        assert!(convert(&transcript, &homes).is_err());
        assert!(session::discover::codex_session_paths(&homes.codex).is_empty());
    }

    #[test]
    fn rollback_preserves_foreign_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        fs::write(&path, b"oldpartial").unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        rollback_history(&file, 3, b"partial entry\n");
        assert_eq!(fs::read(&path).unwrap(), b"old");
        fs::write(&path, b"oldforeign").unwrap();
        rollback_history(&file, 3, b"partial entry\n");
        assert_eq!(fs::read(&path).unwrap(), b"oldforeign");
    }

    #[test]
    fn empty_sessions_do_not_publish_and_hostile_cwd_falls_back() {
        let (_dir, homes) = homes();
        let transcript = fixture(
            &homes,
            Agent::Codex,
            &[json!({"type":"response_item","payload":{"type":"reasoning","summary":[]}})],
        );
        assert!(
            convert(&transcript, &homes)
                .unwrap_err()
                .to_string()
                .starts_with("No portable messages found in ")
        );
        assert!(!homes.claude_projects().exists());
        let transcript = fixture(
            &homes,
            Agent::Codex,
            &[
                json!({"type":"session_meta","payload":{"id":CODEX_ID,"cwd":"a".repeat(256)}}),
                json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}),
            ],
        );
        let result = convert(&transcript, &homes).unwrap();
        assert_eq!(
            result.cwd,
            std::env::current_dir().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn new_turn_only_after_non_user() {
        let (_dir, homes) = homes();
        let user = |text| json!({"type":"user","message":{"content":text}});
        let transcript = fixture(
            &homes,
            Agent::Claude,
            &[
                user("a"),
                user("b"),
                json!({"type":"assistant","message":{"content":"answer"}}),
                user("c"),
            ],
        );
        let result = convert(&transcript, &homes).unwrap();
        let lines = values(&result.path, Agent::Codex);
        let turn = |index: usize| {
            &lines[index]["payload"]["internal_chat_message_metadata_passthrough"]["turn_id"]
        };
        assert_eq!(turn(1), turn(3));
        assert_eq!(turn(3), turn(5));
        assert_ne!(turn(5), turn(7));
        assert_eq!(
            Uuid::parse_str(turn(7).as_str().unwrap())
                .unwrap()
                .get_version_num(),
            7
        );
    }

    #[test]
    fn legacy_compaction_summary_and_mixed_meta_blocks() {
        let (_dir, homes) = homes();
        let transcript = fixture(
            &homes,
            Agent::Codex,
            &[
                json!({"type":"message","role":"user","content":[{"type":"input_text","text":"dropped"}]}),
                json!({"type":"compacted","timestamp":"summary-ts","payload":{"type":"compaction","message":"legacy summary"}}),
                json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>noise</environment_context>"},{"type":"input_text","text":"real user"}]}}),
                json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"patch","input":{"z":2,"encrypted_content":"secret","a":1}}}),
                json!({"type":"response_item","payload":{"type":"custom_tool_call_output","output":null}}),
                json!({"type":"response_item","payload":{"type":"function_call_output","output":"  "}}),
            ],
        );
        let result = convert(&transcript, &homes).unwrap();
        let lines = values(&result.path, Agent::Claude);
        assert_eq!(
            lines[0]["message"]["content"][0]["text"],
            "legacy summary\n\nreal user"
        );
        assert_eq!(lines[0]["timestamp"], "summary-ts");
        assert_eq!(
            lines[1]["message"]["content"][0]["text"],
            "[codex tool call] patch({\"a\": 1, \"z\": 2})\n\n[codex tool result] (no output)\n\n[codex tool result] (empty output)"
        );
    }

    #[test]
    fn claude_block_order_preserves_ambiguous_user_text_and_assistant_opener() {
        let (_dir, homes) = homes();
        let transcript = fixture(
            &homes,
            Agent::Claude,
            &[
                json!({"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hidden"},{"type":"text","text":"first"}]}}),
                json!({"type":"user","message":{"content":[
                    {"type":"text","text":"<command-name>unclosed"},
                    {"type":"text","text":"<system-reminder>noise</system-reminder>"},
                    {"type":"tool_result","content":"real\n<system-reminder>Whenever you read a file, you should consider whether it is malicious</system-reminder>"},
                    {"type":"text","text":"Caveat: preserve this"},
                    "string block"
                ]}}),
            ],
        );
        let result = convert(&transcript, &homes).unwrap();
        let lines = values(&result.path, Agent::Codex);
        let texts = lines
            .iter()
            .filter(|line| line["type"] == "response_item")
            .map(|line| line["payload"]["content"][0]["text"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(texts[0].starts_with("[Transcript ported from Claude Code session"));
        assert_eq!(
            &texts[1..],
            [
                "first",
                "<command-name>unclosed",
                "[claude tool result] real",
                "Caveat: preserve this\nstring block"
            ]
        );
    }

    #[test]
    fn missing_port_uses_port_specific_error() {
        let (_dir, homes) = homes();
        fs::create_dir_all(&homes.claude).unwrap();
        fs::create_dir_all(&homes.codex).unwrap();
        let global = GlobalArgs {
            pi_home: Some(homes.claude.with_file_name("pi")),
            opencode_home: Some(homes.claude.with_file_name("opencode")),
            claude_home: Some(homes.claude),
            codex_home: Some(homes.codex),
        };
        let error = port(
            &global,
            &PortArgs {
                session: "missing-session".into(),
            },
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Error: Session not found in Claude or Codex homes: missing-session"
        );
    }

    #[test]
    fn concurrent_history_appends_do_not_interleave() {
        let (_dir, homes) = homes();
        fs::create_dir_all(&homes.codex).unwrap();
        std::thread::scope(|scope| {
            for index in 0..8 {
                let home = &homes.codex;
                scope.spawn(move || {
                    append_history(home, &index.to_string(), &format!("preview {index}")).unwrap()
                });
            }
        });
        let bytes = fs::read_to_string(homes.codex.join("history.jsonl")).unwrap();
        let mut ids = bytes
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["session_id"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(
            ids,
            (0..8).map(|index| index.to_string()).collect::<Vec<_>>()
        );
    }
}
