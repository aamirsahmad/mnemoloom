//! Antigravity CLI (`agy`) session reader (`~/.gemini/antigravity-cli`,
//! overridden by `AGY_HOME` / `--agy-home`).
//!
//! Each CLI conversation stores a JSONL step transcript at
//! `<home>/brain/<conversation_id>/.system_generated/logs/transcript.jsonl`
//! while session titles, workspace URIs and parent conversation links live in
//! `<home>/conversation_summaries.db`, `<home>/annotations/<id>.pbtxt`, and
//! `<home>/history.jsonl`. Read-only: `mnemoloom` never writes to any file in
//! the `agy` home.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::Agent;
use super::lineage::Derivation;
use super::message::{Message, MessageKind, Role, is_meta_text, preview_text};
use super::meta::{Depth, SessionFile};

/// Metadata for one `agy` conversation from `conversation_summaries.db`,
/// `annotations/<id>.pbtxt`, and `history.jsonl`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SummaryMeta {
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub parent_id: Option<String>,
    pub is_sidechain: bool,
}

/// Path to `<home>/brain/<id>/.system_generated/logs/transcript.jsonl`.
pub fn transcript_path(home: &Path, session_id: &str) -> PathBuf {
    home.join("brain")
        .join(session_id)
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl")
}

/// Extract `<conversation_id>` when `path` is
/// `.../brain/<id>/.system_generated/logs/transcript.jsonl` (or
/// `transcript_full.jsonl`).
pub fn session_id_from_path(path: &Path) -> Option<String> {
    let file = path.file_name()?.to_str()?;
    if file != "transcript.jsonl" && file != "transcript_full.jsonl" {
        return None;
    }
    let logs = path.parent()?;
    if logs.file_name()?.to_str()? != "logs" {
        return None;
    }
    let sys = logs.parent()?;
    if sys.file_name()?.to_str()? != ".system_generated" {
        return None;
    }
    let conv = sys.parent()?;
    let id = conv.file_name()?.to_str()?;
    (!id.is_empty()).then(|| id.to_string())
}

/// Extract `<home>` when `path` is
/// `<home>/brain/<id>/.system_generated/logs/transcript.jsonl` (or
/// `transcript_full.jsonl`).
pub fn home_from_transcript_path(path: &Path) -> Option<PathBuf> {
    let _ = session_id_from_path(path)?;
    let brain = path.parent()?.parent()?.parent()?.parent()?;
    if brain.file_name()?.to_str()? != "brain" {
        return None;
    }
    brain.parent().map(Path::to_path_buf)
}

/// Every `<home>/brain/<id>/.system_generated/logs/transcript.jsonl` sorted
/// by path.
pub fn session_paths(home: &Path) -> Vec<PathBuf> {
    let brain = home.join("brain");
    let Ok(entries) = fs::read_dir(&brain) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| {
            e.path()
                .join(".system_generated")
                .join("logs")
                .join("transcript.jsonl")
        })
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

fn open_ro(db: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let _ = conn.busy_timeout(Duration::from_millis(500));
    Ok(conn)
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Extract the first workspace filesystem path from a JSON array of `file://`
/// URIs (e.g. `["file:///home/user/app"]` -> `Some("/home/user/app")`).
fn parse_workspace_uris(raw: &str) -> Option<String> {
    let arr: Vec<String> = serde_json::from_str(raw).ok()?;
    arr.into_iter().find_map(|uri| {
        let trimmed = uri.trim();
        if let Some(rest) = trimmed.strip_prefix("file://") {
            (!rest.is_empty()).then(|| rest.to_string())
        } else if trimmed.starts_with('/') {
            Some(trimmed.to_string())
        } else {
            None
        }
    })
}

/// Parse `title:"..."` from `<home>/annotations/<id>.pbtxt`.
pub fn read_annotation_title(home: &Path, session_id: &str) -> Option<String> {
    let p = home.join("annotations").join(format!("{session_id}.pbtxt"));
    let text = fs::read_to_string(p).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("title:") {
            let rest = rest.trim();
            if let Some(inner) = rest.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                let unescaped = inner.replace("\\\"", "\"").replace("\\\\", "\\");
                let trimmed = unescaped.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

/// Read workspace paths keyed by lowercased `conversationId` from
/// `<home>/history.jsonl`.
fn read_history_workspaces(home: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(f) = File::open(home.join("history.jsonl")) else {
        return out;
    };
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(cid) = v
            .get("conversationId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(ws) = v
            .get("workspace")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        out.insert(cid.to_lowercase(), ws.to_string());
    }
    out
}

/// Read all summary metadata in `home` keyed by lowercased `conversation_id`.
pub fn summaries(home: &Path) -> Result<HashMap<String, SummaryMeta>, rusqlite::Error> {
    let mut map: HashMap<String, SummaryMeta> = HashMap::new();
    let db = home.join("conversation_summaries.db");
    if db.is_file() {
        let conn = open_ro(&db)?;
        let mut stmt = conn.prepare(
            "SELECT conversation_id, title, workspace_uris, parent_conversation_id, nesting_depth
             FROM conversation_summaries",
        )?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let title = non_empty(r.get::<_, Option<String>>(1)?);
            let uris: Option<String> = r.get(2)?;
            let cwd = uris.as_deref().and_then(parse_workspace_uris);
            let parent_id = non_empty(r.get::<_, Option<String>>(3)?);
            let depth: i64 = r.get::<_, Option<i64>>(4)?.unwrap_or(0);
            let is_sidechain = depth > 0 || parent_id.is_some();
            Ok(SummaryMeta {
                id,
                title,
                cwd,
                parent_id,
                is_sidechain,
            })
        })?;
        for row in rows {
            let row = row?;
            map.insert(row.id.to_lowercase(), row);
        }
    }
    let history_cwds = read_history_workspaces(home);
    for (cid_lc, ws) in history_cwds {
        let entry = map.entry(cid_lc.clone()).or_insert_with(|| SummaryMeta {
            id: cid_lc,
            ..SummaryMeta::default()
        });
        if entry.cwd.is_none() {
            entry.cwd = Some(ws);
        }
    }
    Ok(map)
}

/// Look up metadata for a single `session_id` in `home`.
pub fn summary_for(home: &Path, session_id: &str) -> SummaryMeta {
    let mut meta = SummaryMeta {
        id: session_id.to_string(),
        ..SummaryMeta::default()
    };
    let db = home.join("conversation_summaries.db");
    if db.is_file()
        && let Ok(conn) = open_ro(&db)
        && let Ok(mut stmt) = conn.prepare(
            "SELECT title, workspace_uris, parent_conversation_id, nesting_depth
             FROM conversation_summaries WHERE conversation_id = ?1",
        )
        && let Ok(mut rows) = stmt.query([session_id])
        && let Ok(Some(r)) = rows.next()
    {
        meta.title = non_empty(r.get::<_, Option<String>>(0).ok().flatten());
        let uris: Option<String> = r.get::<_, Option<String>>(1).ok().flatten();
        meta.cwd = uris.as_deref().and_then(parse_workspace_uris);
        meta.parent_id = non_empty(r.get::<_, Option<String>>(2).ok().flatten());
        let depth: i64 = r.get::<_, Option<i64>>(3).ok().flatten().unwrap_or(0);
        meta.is_sidechain = depth > 0 || meta.parent_id.is_some();
    }
    if meta.title.is_none() {
        meta.title = read_annotation_title(home, session_id);
    }
    if meta.cwd.is_none() {
        let cwds = read_history_workspaces(home);
        meta.cwd = cwds.get(&session_id.to_lowercase()).cloned();
    }
    meta
}

/// Extract the user's prompt text from an `agy` `USER_INPUT` step's `content`.
/// Unwraps `<USER_REQUEST>...</USER_REQUEST>` when present and strips IDE/CLI
/// metadata blocks (`<ADDITIONAL_METADATA>`, `<USER_SETTINGS_CHANGE>`).
pub fn clean_user_content(raw: &str) -> String {
    if let Some(start) = raw.find("<USER_REQUEST>")
        && let rest = &raw[start + "<USER_REQUEST>".len()..]
        && let Some(end) = rest.find("</USER_REQUEST>")
    {
        return rest[..end].trim().to_string();
    }
    let mut s = raw.to_string();
    for (open, close) in [
        ("<ADDITIONAL_METADATA>", "</ADDITIONAL_METADATA>"),
        ("<USER_SETTINGS_CHANGE>", "</USER_SETTINGS_CHANGE>"),
    ] {
        while let Some(i) = s.find(open) {
            if let Some(j) = s[i..].find(close) {
                s.replace_range(i..i + j + close.len(), "");
            } else {
                s.truncate(i);
                break;
            }
        }
    }
    s.trim().to_string()
}

fn is_agy_step(v: &Value) -> bool {
    v.get("step_index").and_then(Value::as_u64).is_some()
        && (v.get("type").and_then(Value::as_str).is_some()
            || v.get("source").and_then(Value::as_str).is_some())
}

/// Read `SessionFile` metadata from an `agy` `transcript.jsonl` file.
pub fn read_meta(path: &Path, depth: Depth) -> io::Result<SessionFile> {
    let md = fs::metadata(path)?;
    let session_id = session_id_from_path(path).unwrap_or_else(|| super::path_session_uuid(path));
    let mut sf = SessionFile {
        path: path.to_path_buf(),
        agent: Agent::Agy,
        session_id: session_id.clone(),
        content_id: Some(session_id.clone()),
        cwd: None,
        git_branch: None,
        name: None,
        created: None,
        last_timestamp: None,
        modified: md.modified().unwrap_or(UNIX_EPOCH),
        changed: UNIX_EPOCH
            + Duration::new(
                md.ctime().max(0) as u64,
                md.ctime_nsec().clamp(0, 999_999_999) as u32,
            ),
        size: md.len(),
        line_count: None,
        is_valid: false,
        has_conversation: false,
        is_sidechain: false,
        is_helper: false,
        is_exec_run: false,
        is_legacy_codex: false,
        parent: None,
        first_user_message: None,
        depth,
    };

    let reader = BufReader::new(File::open(path)?);
    let mut lines = 0;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        lines += 1;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if is_agy_step(&v) {
            sf.is_valid = true;
        }
        if let Some(ts) = v
            .get("created_at")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            if sf.created.is_none() {
                sf.created = Some(ts.to_string());
            }
            if depth == Depth::Full {
                sf.last_timestamp = Some(ts.to_string());
            }
        }
        match v.get("type").and_then(Value::as_str) {
            Some("USER_INPUT") => {
                sf.has_conversation = true;
                if sf.first_user_message.is_none()
                    && let Some(raw) = v.get("content").and_then(Value::as_str)
                {
                    let clean = clean_user_content(raw);
                    if !clean.is_empty() && !is_meta_text(&clean) {
                        sf.first_user_message = Some(preview_text(&clean));
                    }
                }
            }
            Some("PLANNER_RESPONSE") => {
                sf.has_conversation = true;
            }
            _ => {}
        }
        if depth == Depth::Header
            && sf.is_valid
            && sf.created.is_some()
            && sf.first_user_message.is_some()
        {
            break;
        }
    }
    if depth == Depth::Full {
        sf.line_count = Some(lines);
    }

    if let Some(home) = home_from_transcript_path(path) {
        let meta = summary_for(&home, &session_id);
        sf.cwd = meta.cwd;
        sf.name = meta.title;
        sf.is_sidechain = meta.is_sidechain;
    }

    Ok(sf)
}

/// Normalize `tool_calls[].args`: in `agy` transcripts each argument value is
/// often a JSON-encoded string literal (e.g. `"\"ls -la\""`). Unwrapping inner
/// JSON values produces clean tool summaries and search index entries.
fn normalize_tool_args(args: &Value) -> Value {
    let Value::Object(map) = args else {
        return args.clone();
    };
    let mut out = serde_json::Map::with_capacity(map.len());
    for (k, v) in map {
        let parsed = match v {
            Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or_else(|_| v.clone()),
            _ => v.clone(),
        };
        out.insert(k.clone(), parsed);
    }
    Value::Object(out)
}

/// Parse one `agy` transcript step into normalized [`Message`]s.
pub fn parse_step(idx: usize, v: &Value) -> Vec<Message> {
    let ts = v
        .get("created_at")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let mut out = Vec::new();
    match v.get("type").and_then(Value::as_str) {
        Some("USER_INPUT") => {
            if let Some(raw) = v.get("content").and_then(Value::as_str) {
                let text = clean_user_content(raw);
                if !text.is_empty() {
                    let mut m = Message::new(idx, out.len(), Role::User, MessageKind::Text, text);
                    m.timestamp.clone_from(&ts);
                    out.push(m);
                }
            }
            if let Some(media) = v.get("media").and_then(Value::as_array) {
                for item in media {
                    if let Some(mime) = item
                        .get("mime_type")
                        .and_then(Value::as_str)
                        .filter(|m| m.starts_with("image/"))
                    {
                        let mut m = Message::new(
                            idx,
                            out.len(),
                            Role::User,
                            MessageKind::Image,
                            format!("[image: {mime}]"),
                        );
                        m.timestamp.clone_from(&ts);
                        out.push(m);
                    }
                }
            }
        }
        Some("PLANNER_RESPONSE") => {
            if let Some(thinking) = v
                .get("thinking")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                let mut m = Message::new(
                    idx,
                    out.len(),
                    Role::Assistant,
                    MessageKind::Thinking,
                    thinking.to_string(),
                );
                m.timestamp.clone_from(&ts);
                out.push(m);
            }
            if let Some(calls) = v.get("tool_calls").and_then(Value::as_array) {
                for tc in calls {
                    let name = tc.get("name").and_then(Value::as_str).unwrap_or("unknown");
                    let input = normalize_tool_args(tc.get("args").unwrap_or(&Value::Null));
                    let mut m = Message::new(
                        idx,
                        out.len(),
                        Role::Assistant,
                        MessageKind::ToolUse,
                        input.to_string(),
                    );
                    m.tool_name = Some(name.to_string());
                    m.tool_input = Some(input);
                    m.timestamp.clone_from(&ts);
                    out.push(m);
                }
            }
            if let Some(text) = v
                .get("content")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                let mut m = Message::new(
                    idx,
                    out.len(),
                    Role::Assistant,
                    MessageKind::Text,
                    text.to_string(),
                );
                m.timestamp.clone_from(&ts);
                out.push(m);
            }
        }
        Some("GENERIC") => {
            if let Some(text) = v
                .get("content")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                let mut m = Message::new(
                    idx,
                    0,
                    Role::Tool,
                    MessageKind::ToolResult,
                    text.to_string(),
                );
                m.is_error = v.get("status").and_then(Value::as_str) == Some("ERROR");
                m.timestamp = ts;
                out.push(m);
            }
        }
        _ => {}
    }
    out
}

/// Count real user prompts in an `agy` transcript.
pub fn count_user_messages(path: &Path) -> usize {
    let Ok(f) = File::open(path) else { return 0 };
    BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter(|line| {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                return false;
            };
            if v.get("type").and_then(Value::as_str) != Some("USER_INPUT") {
                return false;
            }
            v.get("content")
                .and_then(Value::as_str)
                .map(clean_user_content)
                .is_some_and(|t| !t.is_empty() && !is_meta_text(&t))
        })
        .count()
}

/// Parent chain, NEWEST first: the session and each `parent_conversation_id`
/// ancestor in `conversation_summaries.db`; every child link is
/// [`Derivation::Subtask`], the root `Original`. Cycle-safe.
pub fn parent_chain(path: &Path) -> Vec<(PathBuf, Derivation)> {
    let (Some(home), Some(mut id)) = (home_from_transcript_path(path), session_id_from_path(path))
    else {
        return vec![(path.to_path_buf(), Derivation::Original)];
    };
    let Ok(by_id) = summaries(&home) else {
        return vec![(path.to_path_buf(), Derivation::Original)];
    };
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    while seen.insert(id.to_lowercase()) {
        let parent = by_id
            .get(&id.to_lowercase())
            .and_then(|r| r.parent_id.clone());
        let kind = if parent.is_some() {
            Derivation::Subtask
        } else {
            Derivation::Original
        };
        chain.push((transcript_path(&home, &id), kind));
        match parent {
            Some(p) => id = p,
            None => break,
        }
    }
    chain
}

/// Child sessions by parent transcript path (the shape of
/// [`super::lineage::child_index`], for [`super::lineage::descendants`]).
pub fn child_index(home: &Path) -> HashMap<String, Vec<(PathBuf, Derivation)>> {
    let mut out: HashMap<String, Vec<(PathBuf, Derivation)>> = HashMap::new();
    let Ok(by_id) = summaries(home) else {
        return out;
    };
    for row in by_id.values() {
        let Some(pid) = row.parent_id.as_deref() else {
            continue;
        };
        let p = transcript_path(home, &row.id);
        if !p.is_file() {
            continue;
        }
        out.entry(transcript_path(home, pid).to_string_lossy().into_owned())
            .or_default()
            .push((p, Derivation::Subtask));
    }
    for v in out.values_mut() {
        v.sort();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Transcript;
    use crate::session::testutil::{AGY_CHILD_ID, AGY_ID, write_agy};

    #[test]
    fn path_helpers_and_metadata() {
        let d = tempfile::tempdir().unwrap();
        let p = write_agy(d.path(), "/w/app");
        assert_eq!(session_id_from_path(&p).as_deref(), Some(AGY_ID));
        assert_eq!(home_from_transcript_path(&p).as_deref(), Some(d.path()));
        assert_eq!(session_paths(d.path()).len(), 2);

        let sf = read_meta(&p, Depth::Full).unwrap();
        assert_eq!(sf.agent, Agent::Agy);
        assert!(sf.is_valid && sf.has_conversation && !sf.is_sidechain && sf.eligible());
        assert_eq!(sf.session_id, AGY_ID);
        assert_eq!(sf.cwd.as_deref(), Some("/w/app"));
        assert_eq!(sf.name.as_deref(), Some("Agy rate limiter"));
        assert_eq!(sf.first_user_message.as_deref(), Some("add a rate limiter"));
        assert_eq!(sf.created.as_deref(), Some("2026-11-10T04:00:00Z"));
        assert_eq!(sf.last_timestamp.as_deref(), Some("2026-11-10T04:00:05Z"));
        assert_eq!(count_user_messages(&p), 1);

        let child_p = transcript_path(d.path(), AGY_CHILD_ID);
        let child_sf = read_meta(&child_p, Depth::Header).unwrap();
        assert!(child_sf.is_sidechain && !child_sf.eligible());

        let t = Transcript::load(&p, Agent::Agy).unwrap();
        let msgs = t.messages();
        let kinds: Vec<(Role, MessageKind)> = msgs.iter().map(|m| (m.role, m.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                (Role::User, MessageKind::Text),
                (Role::Assistant, MessageKind::Thinking),
                (Role::Assistant, MessageKind::ToolUse),
                (Role::Tool, MessageKind::ToolResult),
                (Role::Assistant, MessageKind::Text),
            ]
        );
        assert_eq!(msgs[0].text, "add a rate limiter");
        assert_eq!(msgs[2].tool_name.as_deref(), Some("run_command"));
        assert_eq!(
            msgs[2]
                .tool_input
                .as_ref()
                .and_then(|v| v.get("CommandLine"))
                .and_then(Value::as_str),
            Some("ls -la")
        );
        assert_eq!(msgs[3].text, "src\n");
        assert_eq!(msgs[4].text, "Done.");

        let chain = parent_chain(&child_p);
        assert_eq!(
            chain,
            vec![
                (child_p.clone(), Derivation::Subtask),
                (p.clone(), Derivation::Original),
            ]
        );
        let children = child_index(d.path());
        assert_eq!(
            children[&*p.to_string_lossy()],
            vec![(child_p, Derivation::Subtask)]
        );
    }
}
