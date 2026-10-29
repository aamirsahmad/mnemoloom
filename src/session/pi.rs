//! pi (pi.dev) sessions: `<pi home>/sessions/<encoded cwd>/<ts>_<uuid>.jsonl`.
//!
//! The first line is a header (`{"type":"session","id","timestamp","cwd"}`);
//! every later entry has `id` / `parentId`, so a file is a tree: editing an
//! earlier prompt starts a branch whose entries point back into the trunk.
//! The active branch is the path from the LAST entry back to the root
//! ([`active_branch`]); displays use it, the search index reads everything.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;

use super::message::{Message, MessageKind, Role, content_text, is_meta_text, preview_text};
use super::meta::{Depth, SessionFile};
use super::{Agent, session_uuid};

/// Every `*.jsonl` under `<home>/sessions` (any depth, symlinks followed).
pub fn session_paths(home: &Path) -> Vec<PathBuf> {
    if home.as_os_str().is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    super::discover::walk_jsonl(&home.join("sessions"), &mut out, &mut visited, &|p| {
        p.extension().is_some_and(|x| x == "jsonl")
    });
    out
}

/// The pi header (`type: session` with an `id`), if `v` is one.
pub fn is_header(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("session")
        && v.get("id").and_then(Value::as_str).is_some()
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Metadata for a pi session file. `Header` stops once the header and the
/// first real user prompt are known; `Full` also reads the name
/// (`session_info.name`), last timestamp and line count.
pub fn read_meta(path: &Path, depth: Depth) -> io::Result<SessionFile> {
    let md = fs::metadata(path)?;
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let mut sf = SessionFile {
        path: path.to_path_buf(),
        agent: Agent::Pi,
        session_id: session_uuid(&name),
        content_id: None,
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
    let reader = BufReader::with_capacity(256 * 1024, File::open(path)?);
    let mut lines = 0;
    for (i, line) in reader.split(b'\n').enumerate() {
        let line = line?;
        lines += 1;
        if depth == Depth::Header && sf.is_valid && sf.first_user_message.is_some() {
            break;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line.trim_ascii()) else {
            continue;
        };
        if i == 0 && is_header(&v) {
            sf.is_valid = true;
            sf.content_id = str_field(&v, "id").map(str::to_string);
            sf.cwd = str_field(&v, "cwd").map(str::to_string);
            sf.created = str_field(&v, "timestamp").map(str::to_string);
            continue;
        }
        if let Some(ts) = str_field(&v, "timestamp") {
            sf.last_timestamp = Some(ts.to_string());
        }
        match str_field(&v, "type") {
            Some("message") => {
                sf.has_conversation = true;
                if sf.first_user_message.is_none() {
                    sf.first_user_message = user_text(&v);
                }
            }
            Some("session_info") => {
                if let Some(n) = str_field(&v, "name")
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                {
                    sf.name = Some(n.to_string());
                }
            }
            _ => {}
        }
    }
    if depth == Depth::Full {
        sf.line_count = Some(lines);
    } else {
        sf.last_timestamp = None;
    }
    Ok(sf)
}

/// First non-meta text of a `user` message entry (preview form).
fn user_text(v: &Value) -> Option<String> {
    let m = v.get("message")?;
    if str_field(m, "role") != Some("user") {
        return None;
    }
    let text = match m.get("content")? {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| str_field(b, "type") == Some("text"))
            .filter_map(|b| str_field(b, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    (!text.trim().is_empty() && !is_meta_text(&text)).then(|| preview_text(&text))
}

/// Indexes (into `entries`) of the active branch: the last entry with an
/// `id`, then its `parentId` chain back to the root. Entries without an id
/// (the header) are always kept. Cycle-safe; an unknown parent ends the walk.
pub fn active_branch<'a>(entries: impl IntoIterator<Item = Option<&'a Value>>) -> HashSet<usize> {
    let mut by_id: HashMap<&str, (usize, Option<&str>)> = HashMap::new();
    let mut keep = HashSet::new();
    let mut leaf = None;
    for (i, v) in entries.into_iter().enumerate() {
        let Some(v) = v else { continue };
        match str_field(v, "id").filter(|_| !is_header(v)) {
            Some(id) => {
                by_id.insert(id, (i, str_field(v, "parentId")));
                leaf = Some(id);
            }
            None => {
                keep.insert(i);
            }
        }
    }
    let mut seen = HashSet::new();
    let mut cur = leaf;
    while let Some(id) = cur.filter(|id| seen.insert(*id)) {
        let Some(&(i, parent)) = by_id.get(id) else {
            break;
        };
        keep.insert(i);
        cur = parent;
    }
    keep
}

/// Messages of one pi entry (empty for non-message entries).
pub fn parse_entry(idx: usize, v: &Value) -> Vec<Message> {
    let ts = str_field(v, "timestamp").map(str::to_string);
    let mut out = match str_field(v, "type") {
        Some("message") => v
            .get("message")
            .map(|m| message(idx, m))
            .unwrap_or_default(),
        Some(ty @ ("compaction" | "branch_summary")) => {
            let kind = if ty == "compaction" {
                MessageKind::Compaction
            } else {
                MessageKind::Text
            };
            let text = str_field(v, "summary").unwrap_or("").to_string();
            let mut m = Message::new(idx, 0, Role::System, kind, text);
            m.is_meta = true;
            vec![m]
        }
        _ => Vec::new(),
    };
    for m in &mut out {
        m.timestamp.clone_from(&ts);
    }
    out
}

fn message(idx: usize, m: &Value) -> Vec<Message> {
    let role = match str_field(m, "role") {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        Some("toolResult") => Role::Tool,
        Some("system") => Role::System,
        _ => return Vec::new(),
    };
    if role == Role::Tool {
        let mut r = Message::new(
            idx,
            0,
            Role::Tool,
            MessageKind::ToolResult,
            m.get("content").map(content_text).unwrap_or_default(),
        );
        r.tool_id = str_field(m, "toolCallId").map(str::to_string);
        r.tool_name = str_field(m, "toolName").map(str::to_string);
        r.is_error = m.get("isError").and_then(Value::as_bool) == Some(true);
        return vec![r];
    }
    let mut out = match m.get("content") {
        Some(Value::String(s)) => vec![Message::new(idx, 0, role, MessageKind::Text, s.clone())],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .enumerate()
            .map(|(bi, b)| block(idx, bi, role, b))
            .collect(),
        _ => Vec::new(),
    };
    for msg in &mut out {
        msg.is_meta = role == Role::System
            || (msg.kind == MessageKind::Text && role == Role::User && is_meta_text(&msg.text));
    }
    out
}

fn block(idx: usize, bi: usize, role: Role, b: &Value) -> Message {
    let text = |k: &str| str_field(b, k).unwrap_or("").to_string();
    match str_field(b, "type") {
        Some("text") => Message::new(idx, bi, role, MessageKind::Text, text("text")),
        Some("thinking") => Message::new(idx, bi, role, MessageKind::Thinking, text("thinking")),
        Some("toolCall") => {
            let input = b.get("arguments").cloned();
            let mut m = Message::new(
                idx,
                bi,
                Role::Assistant,
                MessageKind::ToolUse,
                input.as_ref().map(Value::to_string).unwrap_or_default(),
            );
            m.tool_name = str_field(b, "name").map(str::to_string);
            m.tool_id = str_field(b, "id").map(str::to_string);
            m.tool_input = input;
            m
        }
        Some("image") => Message::new(idx, bi, role, MessageKind::Image, String::new()),
        _ => Message::new(idx, bi, role, MessageKind::Other, String::new()),
    }
}

/// Count of `user` message entries (all branches), for `info`.
pub fn count_user_messages(path: &Path) -> usize {
    let Ok(f) = File::open(path) else { return 0 };
    BufReader::new(f)
        .split(b'\n')
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_slice::<Value>(l.trim_ascii()).ok())
        .filter(|v| {
            str_field(v, "type") == Some("message")
                && v.pointer("/message/role").and_then(Value::as_str) == Some("user")
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    #[test]
    fn metadata_from_header_and_entries() {
        let d = tempfile::tempdir().unwrap();
        let p = write_pi(
            d.path(),
            PI_ID,
            "/home/alice/src/api",
            &pi_lines(PI_ID, "/home/alice/src/api"),
        );
        assert_eq!(session_paths(d.path()), vec![p.clone()]);
        let sf = read_meta(&p, Depth::Full).unwrap();
        assert_eq!(sf.session_id, PI_ID);
        assert_eq!(sf.content_id.as_deref(), Some(PI_ID));
        assert_eq!(sf.cwd.as_deref(), Some("/home/alice/src/api"));
        assert_eq!(sf.created.as_deref(), Some("2026-10-22T04:45:14.324Z"));
        assert_eq!(
            sf.last_timestamp.as_deref(),
            Some("2026-10-22T04:47:00.000Z")
        );
        assert_eq!(sf.name.as_deref(), Some("rate limiter"));
        assert_eq!(sf.first_user_message.as_deref(), Some("add a rate limiter"));
        assert_eq!(sf.line_count, Some(pi_lines(PI_ID, "/x").len()));
        assert!(sf.is_valid && sf.has_conversation && sf.eligible());
        let h = read_meta(&p, Depth::Header).unwrap();
        assert_eq!((h.name, h.line_count), (None, None));
        assert_eq!(count_user_messages(&p), 3);
    }

    #[test]
    fn active_branch_follows_last_leaf() {
        let values: Vec<Value> = pi_lines(PI_ID, "/x")
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let keep = active_branch(values.iter().map(Some));
        let ids: Vec<&str> = (0..values.len())
            .filter(|i| keep.contains(i))
            .filter_map(|i| values[i].get("id").and_then(Value::as_str))
            .collect();
        // u2/a2 (the abandoned branch) are off the active path.
        assert_eq!(
            ids,
            [
                PI_ID, "m0", "sys", "u1", "a1", "t1", "a1b", "u3", "a3", "s1"
            ]
        );
    }

    #[test]
    fn parses_message_kinds() {
        let values: Vec<Value> = pi_lines(PI_ID, "/x")
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let msgs: Vec<Message> = values
            .iter()
            .enumerate()
            .flat_map(|(i, v)| parse_entry(i, v))
            .collect();
        let call = msgs
            .iter()
            .find(|m| m.kind == MessageKind::ToolUse)
            .unwrap();
        assert_eq!(call.tool_name.as_deref(), Some("bash"));
        assert_eq!(call.tool_input.as_ref().unwrap()["command"], "ls");
        let result = msgs
            .iter()
            .find(|m| m.kind == MessageKind::ToolResult)
            .unwrap();
        assert_eq!(
            (result.text.as_str(), result.tool_id.as_deref()),
            ("src\n", Some("call_1"))
        );
        assert!(
            msgs.iter()
                .any(|m| m.kind == MessageKind::Thinking && m.text == "plan")
        );
        assert!(msgs.iter().any(|m| m.role == Role::System && m.is_meta));
        assert_eq!(
            msgs[1].timestamp.as_deref(),
            Some("2026-10-22T04:45:15.000Z")
        );
    }
}
