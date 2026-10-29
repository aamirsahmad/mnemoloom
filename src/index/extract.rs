//! Per-file extraction of an index document.
//!
//! Identity (ids, cwd, branch, flags, lineage, created) comes from
//! [`SessionFile::read`]; searchable content, the genuine user-message count
//! and the first/last message previews come from one pass over
//! [`message::parse_line`] output (Claude / Codex raw lines) or over
//! [`Transcript::messages`] (pi / opencode).

use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::session::message::{self, Message, MessageKind, Role, is_meta_text, preview_text};
use crate::session::transcript::Line;
use crate::session::{Agent, Depth, Derivation, SessionFile, Transcript, pi, timefmt};

/// Max chars of a tool result that are indexed.
const TOOL_RESULT_CHARS: usize = 500;
/// `modified` is the last timestamp found among this many trailing lines.
const TAIL_LINES: usize = 64;

/// A (role, preview) pair; empty strings when absent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Preview {
    pub role: String,
    pub content: String,
}

/// Everything stored for one session file.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionDoc {
    pub session_id: String,
    pub agent: Agent,
    pub project: String,
    pub branch: String,
    pub cwd: String,
    pub created: String,
    pub modified: String,
    /// Epoch ms of `modified` (0 when unparseable).
    pub modified_ts: u64,
    /// Genuine user-message count.
    pub lines: i64,
    pub export_path: String,
    pub first_msg: Preview,
    pub last_msg: Preview,
    pub first_user_msg_content: String,
    /// `""`, `"trimmed"` or `"continued"`.
    pub derivation_type: String,
    pub is_sidechain: bool,
    pub is_exec_run: bool,
    pub custom_title: String,
    pub content: String,
    /// SDK/headless helper session (excluded from the index).
    pub is_helper: bool,
    /// Whether the file is a resumable session (invalid files are excluded).
    pub is_valid: bool,
}

/// Extract the index document for `path` (an opencode virtual path for
/// [`Agent::OpenCode`]).
pub fn extract(path: &Path, agent: Agent) -> io::Result<SessionDoc> {
    if matches!(agent, Agent::Pi | Agent::OpenCode) {
        return extract_messages(path, agent);
    }
    let sf = SessionFile::read(path, agent, Depth::Header)?;
    let bytes = fs::read(path)?;
    let mut acc = Accumulator::default();
    for line in bytes.split(|&b| b == b'\n') {
        if line.trim_ascii().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        acc.visit(agent, &v);
    }
    let modified = tail_timestamp(&bytes);
    Ok(document(path, sf, acc, modified))
}

/// pi / opencode: every message is indexed (all pi branches); the count
/// and previews follow the conversation as shown (pi's active branch).
fn extract_messages(path: &Path, agent: Agent) -> io::Result<SessionDoc> {
    let mut sf = SessionFile::read(path, agent, Depth::Header)?;
    let t = Transcript::load(path, agent).map_err(io::Error::other)?;
    let shown = (agent == Agent::Pi).then(|| pi::active_branch(t.lines.iter().map(Line::value)));
    let mut acc = Accumulator::default();
    acc.messages(&t.messages(), |i| {
        shown.as_ref().is_none_or(|s| s.contains(&i))
    });
    // opencode: the session title; pi: the last `session_info.name` (the
    // header-depth read stops before it).
    acc.custom_title = match agent {
        Agent::Pi => t
            .lines
            .iter()
            .rev()
            .filter_map(Line::value)
            .filter(|v| v.get("type").and_then(Value::as_str) == Some("session_info"))
            .filter_map(|v| v.get("name")?.as_str().map(str::trim))
            .find(|n| !n.is_empty())
            .map(str::to_string),
        _ => sf.name.take(),
    };
    let modified = sf.last_timestamp.clone().or_else(|| {
        t.lines.iter().rev().filter_map(Line::value).find_map(|v| {
            v.get("timestamp")?
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
    });
    Ok(document(path, sf, acc, modified))
}

fn document(
    path: &Path,
    sf: SessionFile,
    acc: Accumulator,
    modified: Option<String>,
) -> SessionDoc {
    let agent = sf.agent;
    let modified = modified.unwrap_or_else(|| timefmt::iso_local_offset(sf.modified));
    let created = sf.created.clone().unwrap_or_else(|| {
        let born = fs::metadata(path)
            .and_then(|m| m.created())
            .unwrap_or(sf.modified);
        timefmt::iso_local_offset(born)
    });
    let cwd = sf.cwd.clone().unwrap_or_default();
    SessionDoc {
        session_id: sf.session_id.clone(),
        agent,
        project: Path::new(&cwd)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        branch: sf.git_branch.clone().unwrap_or_default(),
        cwd,
        created,
        modified_ts: epoch_ms(&modified),
        modified,
        lines: acc.user_count,
        export_path: path.to_string_lossy().into_owned(),
        first_msg: acc.first.unwrap_or_default(),
        last_msg: acc.last.unwrap_or_default(),
        first_user_msg_content: acc.first_user.unwrap_or_default(),
        derivation_type: match sf.derivation() {
            Derivation::Original => String::new(),
            d => d.as_str().to_string(),
        },
        is_sidechain: sf.is_sidechain,
        is_exec_run: sf.is_exec_run,
        custom_title: acc.custom_title.unwrap_or_default(),
        content: acc.content.join("\n\n"),
        is_helper: sf.is_helper,
        is_valid: sf.is_valid,
    }
}

/// Epoch milliseconds of an ISO timestamp (0 when unparseable).
pub fn epoch_ms(ts: &str) -> u64 {
    timefmt::parse_iso(ts)
        .map(|d| d.timestamp_millis().max(0) as u64)
        .unwrap_or(0)
}

/// Epoch milliseconds of a `SystemTime`.
pub fn system_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Last non-empty `timestamp` among the trailing lines.
fn tail_timestamp(bytes: &[u8]) -> Option<String> {
    bytes
        .rsplit(|&b| b == b'\n')
        .filter(|l| !l.trim_ascii().is_empty())
        .take(TAIL_LINES)
        .find_map(|l| {
            let v: Value = serde_json::from_slice(l).ok()?;
            v.get("timestamp")?
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
}

#[derive(Default)]
struct Accumulator {
    content: Vec<String>,
    user_count: i64,
    first: Option<Preview>,
    last: Option<Preview>,
    first_user: Option<String>,
    custom_title: Option<String>,
    /// Codex: timestamp of the first user message (never counted: it is
    /// the injected environment context).
    codex_first_user_ts: Option<Option<String>>,
}

impl Accumulator {
    fn visit(&mut self, agent: Agent, v: &Value) {
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        let title = match ty {
            "custom-title" => v.get("customTitle"),
            "agent-name" => v.get("agentName"),
            _ => None,
        };
        if let Some(t) = title.and_then(Value::as_str).filter(|t| !t.is_empty()) {
            self.custom_title = Some(t.to_string());
            return;
        }
        let msgs = message::parse_line(agent, 0, v);
        match agent {
            Agent::Claude if ty == "user" || ty == "assistant" => self.claude(ty, v, &msgs),
            Agent::Codex if ty == "response_item" => {
                if let Some(role) =
                    v.pointer("/payload/role")
                        .and_then(Value::as_str)
                        .filter(|_| {
                            v.pointer("/payload/type").and_then(Value::as_str) == Some("message")
                        })
                {
                    self.codex(role, &msgs);
                }
            }
            _ => {}
        }
    }

    fn claude(&mut self, role: &str, v: &Value, msgs: &[Message]) {
        let mut text = String::new();
        for m in msgs {
            let piece = match m.kind {
                MessageKind::Text => m.text.clone(),
                MessageKind::ToolUse => tool_use_text(m),
                MessageKind::ToolResult => m.text.chars().take(TOOL_RESULT_CHARS).collect(),
                _ => continue,
            };
            text.push_str(&piece);
            if !v.pointer("/message/content").is_some_and(Value::is_string) {
                text.push('\n');
            }
        }
        self.push_content(role, &text);
        let line_meta = v.get("isMeta").and_then(Value::as_bool) == Some(true);
        if role == "user"
            && msgs
                .first()
                .is_some_and(|m| m.kind != MessageKind::ToolResult && !m.is_meta)
        {
            self.user_count += 1;
        }
        let Some(preview) = msgs
            .iter()
            .find(|m| m.kind == MessageKind::Text && !m.text.trim().is_empty())
            .map(|m| m.text.as_str())
        else {
            return;
        };
        let meta = line_meta || is_meta_text(preview);
        self.note_preview(role, preview, role == "user" && !meta);
    }

    fn codex(&mut self, role: &str, msgs: &[Message]) {
        let texts: Vec<&Message> = msgs
            .iter()
            .filter(|m| m.kind == MessageKind::Text)
            .collect();
        let joined: String = texts.iter().map(|m| format!("{}\n", m.text)).collect();
        self.push_content(role, &joined);
        if !matches!(role, "user" | "assistant") {
            return;
        }
        let visible: Vec<&str> = texts
            .iter()
            .filter(|m| !(m.text.trim().is_empty() || role == "user" && is_meta_text(&m.text)))
            .map(|m| m.text.as_str())
            .collect();
        if role == "user" {
            let ts = msgs.first().and_then(|m| m.timestamp.clone());
            match &self.codex_first_user_ts {
                None => self.codex_first_user_ts = Some(ts),
                Some(first) if *first != ts && !is_meta_text(&joined) => self.user_count += 1,
                Some(_) => {}
            }
        }
        if !visible.is_empty() {
            self.note_preview(role, &visible.join("\n"), role == "user");
        }
    }

    /// Message-model pass: runs of blocks from one line and role form one
    /// content entry; `shown(line)` limits the count and previews.
    fn messages(&mut self, msgs: &[Message], shown: impl Fn(usize) -> bool) {
        for group in msgs.chunk_by(|a, b| a.line_index == b.line_index && a.role == b.role) {
            let (line, role) = (group[0].line_index, group[0].role);
            let pieces: Vec<String> = group
                .iter()
                .filter_map(|m| match m.kind {
                    MessageKind::Text => Some(m.text.clone()),
                    MessageKind::ToolUse => Some(tool_use_text(m)),
                    MessageKind::ToolResult => {
                        Some(m.text.chars().take(TOOL_RESULT_CHARS).collect())
                    }
                    _ => None,
                })
                .collect();
            self.push_content(role.as_str(), &pieces.join("\n"));
            if !shown(line) || !matches!(role, Role::User | Role::Assistant) {
                continue;
            }
            let visible: Vec<&str> = group
                .iter()
                .filter(|m| m.kind == MessageKind::Text && !m.is_meta && !m.text.trim().is_empty())
                .map(|m| m.text.as_str())
                .collect();
            if visible.is_empty() {
                continue;
            }
            if role == Role::User {
                self.user_count += 1;
            }
            self.note_preview(role.as_str(), &visible.join("\n"), role == Role::User);
        }
    }

    fn push_content(&mut self, role: &str, text: &str) {
        let t = text.trim();
        if !t.is_empty() {
            self.content.push(format!("[{role}] {t}"));
        }
    }

    fn note_preview(&mut self, role: &str, text: &str, genuine_user: bool) {
        let p = Preview {
            role: role.to_string(),
            content: preview_text(text),
        };
        if genuine_user && self.first_user.is_none() {
            self.first_user = Some(p.content.clone());
        }
        if self.first.is_none() {
            self.first = Some(p.clone());
        }
        self.last = Some(p);
    }
}

/// `[Tool: <name>]` then every non-empty string input value, one per line.
pub fn tool_use_text(m: &Message) -> String {
    let mut out = format!("[Tool: {}]", m.tool_name.as_deref().unwrap_or(""));
    if let Some(Value::Object(input)) = &m.tool_input {
        for s in input
            .values()
            .filter_map(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            out.push('\n');
            out.push_str(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    #[test]
    fn claude_document() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_claude(
            dir.path(),
            CLAUDE_ID,
            "/w/proj",
            &claude_lines(CLAUDE_ID, "/w/proj"),
        );
        let d = extract(&p, Agent::Claude).unwrap();
        assert_eq!(d.session_id, CLAUDE_ID);
        assert_eq!((d.project.as_str(), d.branch.as_str()), ("proj", "main"));
        assert_eq!(d.lines, 1);
        assert_eq!(
            d.first_user_msg_content,
            "fix the failing spec in user_spec.rb"
        );
        assert_eq!(d.first_msg.content, "<command-name>/clear</command-name>");
        assert_eq!(
            (d.last_msg.role.as_str(), d.last_msg.content.as_str()),
            ("assistant", "Fixed.")
        );
        assert_eq!(d.custom_title, "renamed-later");
        assert_eq!(d.created, "2026-10-27T03:28:25.917Z");
        assert_eq!(d.modified, "2026-10-27T03:30:00.000Z");
        assert_eq!(d.modified_ts, 1_793_071_800_000);
        assert!(d.content.contains("[user] fix the failing spec"));
        assert!(
            d.content
                .contains("[assistant] [Tool: Bash]\nbundle exec rspec\nRun spec")
        );
        assert!(d.content.contains("[user] F.\nFailures:"));
        assert!(d.is_valid && !d.is_helper && !d.is_sidechain);
        assert_eq!(d.derivation_type, "");
    }

    #[test]
    fn codex_document() {
        let dir = tempfile::tempdir().unwrap();
        let lines = codex_lines(CODEX_ID, "/w/cx", "exec");
        let p = write_codex(dir.path(), CODEX_ID, "2026-05-07T12-57-30", &lines);
        let d = extract(&p, Agent::Codex).unwrap();
        assert_eq!(d.session_id, CODEX_ID);
        assert_eq!(d.branch, "fix/gql-timeout");
        assert!(d.is_exec_run);
        // The first user message (environment context) is never counted.
        assert_eq!(d.lines, 1);
        assert_eq!(d.first_user_msg_content, "hello codex");
        assert_eq!(d.first_msg.content, "hello codex");
        assert_eq!(d.last_msg.content, "Done.");
        assert_eq!(d.created, "2026-05-07T16:58:02.928Z");
        assert!(
            d.content
                .contains("[developer] <permissions instructions>x")
        );
        assert!(d.content.contains("[user] hello codex"));
        assert!(d.content.contains("[assistant] Done."));
        assert!(!d.content.contains("exec_command"));
    }

    #[test]
    fn pi_document_indexes_every_branch() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_pi(dir.path(), PI_ID, "/w/pi", &pi_lines(PI_ID, "/w/pi"));
        let d = extract(&p, Agent::Pi).unwrap();
        assert_eq!((d.session_id.as_str(), d.agent), (PI_ID, Agent::Pi));
        assert_eq!((d.project.as_str(), d.cwd.as_str()), ("pi", "/w/pi"));
        assert_eq!(d.custom_title, "rate limiter");
        assert_eq!(d.created, "2026-10-22T04:45:14.324Z");
        assert_eq!(d.modified, "2026-10-22T04:47:00.000Z");
        // Count and previews follow the active branch (u1, u3) ...
        assert_eq!(d.lines, 2);
        assert_eq!(d.first_user_msg_content, "add a rate limiter");
        assert_eq!(d.first_msg.content, "add a rate limiter");
        assert_eq!(
            (d.last_msg.role.as_str(), d.last_msg.content.as_str()),
            ("assistant", "Token bucket added.")
        );
        // ... the content has every branch, tool calls and results.
        assert!(d.content.contains("[user] try redis"));
        assert!(d.content.contains("[assistant] Using redis."));
        assert!(d.content.contains("[user] use a token bucket"));
        assert!(d.content.contains("[assistant] [Tool: bash]\nls"));
        assert!(d.content.contains("[tool] src"));
        assert!(!d.content.contains("plan"), "thinking is not indexed");
        assert!(d.is_valid && !d.is_sidechain && d.derivation_type.is_empty());
    }

    #[test]
    fn opencode_documents() {
        let dir = tempfile::tempdir().unwrap();
        let db = write_opencode_db(&dir.path().join("opencode.db"), "/w/oc", 1_792_644_400_000);
        let d = extract(
            &crate::session::opencode::virtual_path(&db, OPENCODE_ID),
            Agent::OpenCode,
        )
        .unwrap();
        assert_eq!(
            (d.session_id.as_str(), d.agent),
            (OPENCODE_ID, Agent::OpenCode)
        );
        assert_eq!(d.export_path, format!("{}#{OPENCODE_ID}", db.display()));
        assert_eq!(
            (d.project.as_str(), d.custom_title.as_str()),
            ("oc", "Rate limiter")
        );
        assert_eq!(d.modified_ts, 1_792_644_400_000);
        assert_eq!(d.lines, 1);
        assert_eq!(d.first_user_msg_content, "add a rate limiter");
        assert_eq!(d.last_msg.content, "Done.");
        assert!(d.content.contains("[user] add a rate limiter"));
        assert!(d.content.contains("[assistant] [Tool: bash]\nls"));
        assert!(d.content.contains("[tool] src"));
        assert!(d.content.contains("[assistant] Done."));
        assert!(!d.is_sidechain);
        let child = extract(
            &crate::session::opencode::virtual_path(&db, OPENCODE_CHILD_ID),
            Agent::OpenCode,
        )
        .unwrap();
        assert!(child.is_sidechain);
        assert_eq!(child.first_user_msg_content, "explore the code");
    }

    #[test]
    fn tool_result_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = claude_lines(CLAUDE_ID, "/w");
        let long = "x".repeat(900);
        lines[6] = lines[6].replace("F.\\nFailures:", &long);
        let p = write_claude(dir.path(), CLAUDE_ID, "/w", &lines);
        let d = extract(&p, Agent::Claude).unwrap();
        assert!(d.content.contains(&"x".repeat(500)));
        assert!(!d.content.contains(&"x".repeat(501)));
    }
}
