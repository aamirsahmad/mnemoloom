//! Flattening and metadata extraction over the shared transcript/message model.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::session::{Agent, Message, MessageKind, Role, Transcript, message, path_session_uuid};

use super::noise::{genuine_user_text, strip_reminders};
use super::text::{has_encrypted, stringify, strip_encrypted, text_blocks};

#[derive(Debug, Default)]
pub(super) struct Metadata {
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub source_id: Option<String>,
    pub timestamp: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FlatMessage {
    pub role: Role,
    pub text: String,
    pub timestamp: Option<String>,
}

pub(super) fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn nonempty(value: &Value, key: &str) -> Option<String> {
    string(value, key)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub(super) fn metadata(transcript: &Transcript) -> Metadata {
    let mut meta = Metadata::default();
    let mut harvested = false;
    let mut authoritative = false;
    let file_id = path_session_uuid(&transcript.path);
    for value in transcript.lines.iter().filter_map(|line| line.value()) {
        if transcript.agent == Agent::Claude {
            for (target, key) in [
                (&mut meta.cwd, "cwd"),
                (&mut meta.branch, "gitBranch"),
                (&mut meta.source_id, "sessionId"),
                (&mut meta.timestamp, "timestamp"),
            ] {
                if target.is_none() {
                    *target = nonempty(value, key);
                }
            }
            continue;
        }
        if value["type"] == "session_meta" {
            let payload = &value["payload"];
            if !payload.is_object() {
                continue;
            }
            let id = nonempty(payload, "id").or_else(|| nonempty(payload, "session_id"));
            let is_authoritative = id.as_deref() == Some(file_id.as_str());
            if is_authoritative && !authoritative {
                meta = Metadata::default();
                authoritative = true;
            } else if harvested {
                continue;
            }
            meta.cwd = nonempty(payload, "cwd");
            meta.branch = nonempty(&payload["git"], "branch");
            meta.source_id = id;
            meta.timestamp = string(value, "timestamp")
                .or_else(|| string(payload, "timestamp"))
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            harvested = true;
        } else if !authoritative
            && value["type"].is_null()
            && value.get("record_type").is_none()
            && value.get("id").is_some()
            && value.get("timestamp").is_some()
        {
            if let Some(id) = nonempty(value, "id") {
                meta.source_id = Some(id);
            }
            if meta.timestamp.is_none() {
                meta.timestamp = string(value, "timestamp").map(str::to_string);
            }
            if let Some(branch) = nonempty(&value["git"], "branch") {
                meta.branch = Some(branch);
            }
        } else if !authoritative
            && meta.cwd.is_none()
            && let Some(payload) = item_payload(value)
            && payload["type"] == "message"
        {
            let text = match &payload["content"] {
                Value::String(text) => text.trim().to_string(),
                content => text_blocks(content).join("\n"),
            };
            if text.trim_start().starts_with("<environment_context>") {
                meta.cwd = environment_cwd(&text);
            }
        }
    }
    meta
}

fn environment_cwd(text: &str) -> Option<String> {
    if let Some((_, rest)) = text.split_once("<cwd>")
        && let Some((cwd, _)) = rest.split_once("</cwd>")
        && !cwd.trim().is_empty()
    {
        return Some(cwd.trim().to_string());
    }
    text.lines().find_map(|line| {
        line.trim_start()
            .strip_prefix("Current working directory:")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

pub(super) fn flatten(transcript: &Transcript, meta: &Metadata) -> Vec<FlatMessage> {
    match transcript.agent {
        Agent::Claude => flatten_claude(transcript, meta.timestamp.as_deref()),
        Agent::Codex => flatten_codex(transcript, meta.timestamp.as_deref()),
        // Refused by `port` (`Agent::require_rewritable`).
        Agent::Pi | Agent::OpenCode => Vec::new(),
    }
}

fn flatten_claude(transcript: &Transcript, fallback: Option<&str>) -> Vec<FlatMessage> {
    let mut by_line: HashMap<usize, Vec<Message>> = HashMap::new();
    for msg in transcript.messages() {
        by_line.entry(msg.line_index).or_default().push(msg);
    }
    let mut output = Vec::new();
    for (index, line) in transcript.lines.iter().enumerate() {
        let Some(value) = line.value() else {
            continue;
        };
        let pairs = claude_line(value, by_line.get(&index).map_or(&[], Vec::as_slice));
        if value["type"] == "user"
            && value["isCompactSummary"] == true
            && pairs.iter().any(|(_, text)| !text.trim().is_empty())
        {
            output.clear();
        }
        let timestamp = string(value, "timestamp")
            .filter(|s| !s.is_empty())
            .or(fallback)
            .map(str::to_string);
        output.extend(
            pairs
                .into_iter()
                .filter(|(_, text)| !text.trim().is_empty())
                .map(|(role, text)| FlatMessage {
                    role,
                    text,
                    timestamp: timestamp.clone(),
                }),
        );
    }
    output
}

fn claude_line(value: &Value, messages: &[Message]) -> Vec<(Role, String)> {
    if value["isSidechain"] == true {
        return Vec::new();
    }
    let role = match string(value, "type") {
        Some("user") if value["isMeta"] != true => Role::User,
        Some("assistant") => Role::Assistant,
        _ => return Vec::new(),
    };
    let content = &value["message"]["content"];
    if let Some(text) = content.as_str() {
        return if !text.trim().is_empty() && (role != Role::User || genuine_user_text(text)) {
            vec![(role, text.to_string())]
        } else {
            Vec::new()
        };
    }
    let Some(blocks) = content.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut parts = Vec::new();
    for msg in messages {
        let block = &blocks[msg.block_index];
        match msg.kind {
            MessageKind::Text => {
                if role == Role::Assistant && !block.is_object() {
                    continue;
                }
                if !msg.text.trim().is_empty()
                    && (role != Role::User || genuine_user_text(&msg.text))
                {
                    parts.push(msg.text.clone());
                }
            }
            MessageKind::ToolUse if role == Role::Assistant => {
                flush(&mut out, &mut parts, role);
                let name = string(block, "name")
                    .filter(|s| !s.is_empty())
                    .unwrap_or("unknown");
                out.push((
                    Role::Assistant,
                    format!("[claude tool call] {name}({})", stringify(&block["input"])),
                ));
            }
            MessageKind::ToolResult if role == Role::User => {
                flush(&mut out, &mut parts, role);
                let raw = &block["content"];
                let text = stringify(&strip_reminders(raw));
                let text = empty_output(text, raw);
                out.push((Role::Assistant, format!("[claude tool result] {text}")));
            }
            _ => {}
        }
    }
    flush(&mut out, &mut parts, role);
    out
}

fn flush(output: &mut Vec<(Role, String)>, parts: &mut Vec<String>, role: Role) {
    if !parts.is_empty() {
        output.push((role, parts.join("\n")));
        parts.clear();
    }
}

fn empty_output(text: String, raw: &Value) -> String {
    if text.is_empty() {
        if raw.is_null() {
            "(no output)".into()
        } else {
            "(empty output)".into()
        }
    } else {
        text
    }
}

fn item_payload(value: &Value) -> Option<&Value> {
    match string(value, "type") {
        Some("response_item") => value.get("payload").filter(|p| p.is_object()),
        Some(
            "message"
            | "agent_message"
            | "reasoning"
            | "function_call"
            | "function_call_output"
            | "custom_tool_call"
            | "custom_tool_call_output",
        ) if value.get("payload").is_none() => Some(value),
        _ => None,
    }
}

fn compaction(value: &Value) -> bool {
    value["type"] == "compacted"
        && (value["payload"]["replacement_history"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
            || string(&value["payload"], "message").is_some_and(|s| !s.trim().is_empty()))
}

fn goal_text(payload: &Value) -> Option<String> {
    if payload["type"] != "message" || payload["role"] != "user" {
        return None;
    }
    let parts = text_blocks(&payload["content"]);
    if parts.is_empty()
        || !parts.iter().all(|part| {
            let text = part.trim_start();
            text.starts_with("<codex_internal_context ")
                || text.starts_with("<codex_internal_context>")
        })
    {
        return None;
    }
    Some(parts.join("\n").trim().to_string())
}

fn flatten_codex(transcript: &Transcript, fallback: Option<&str>) -> Vec<FlatMessage> {
    let boundary = transcript
        .lines
        .iter()
        .rposition(|line| line.value().is_some_and(compaction));
    let mut items: Vec<(&Value, Option<&str>, bool)> = Vec::new();
    for (index, line) in transcript
        .lines
        .iter()
        .enumerate()
        .skip(boundary.unwrap_or(0))
    {
        let Some(value) = line.value() else {
            continue;
        };
        let timestamp = string(value, "timestamp")
            .filter(|s| !s.is_empty())
            .or(fallback);
        if Some(index) == boundary {
            if let Some(history) = value["payload"]["replacement_history"].as_array() {
                items.extend(
                    history
                        .iter()
                        .filter(|item| item.is_object())
                        .map(|item| (item, timestamp, false)),
                );
            }
            // The legacy summary is emitted after replacement history.
            if string(&value["payload"], "message").is_some_and(|s| !s.trim().is_empty()) {
                items.push((&value["payload"], timestamp, true));
            }
        } else if let Some(payload) = item_payload(value) {
            items.push((payload, timestamp, false));
        }
    }
    let last_goal = items
        .iter()
        .rposition(|(item, _, summary)| !summary && goal_text(item).is_some());
    items
        .into_iter()
        .enumerate()
        .filter_map(|(index, (item, timestamp, summary))| {
            let pair = if Some(index) == last_goal {
                goal_text(item).map(|text| (Role::User, format!("[codex goal context]\n{text}")))
            } else if summary {
                string(item, "message").map(|text| (Role::User, text.to_string()))
            } else {
                codex_item(item)
            };
            pair.map(|(role, text)| FlatMessage {
                role,
                text,
                timestamp: timestamp.map(str::to_string),
            })
        })
        .collect()
}

fn codex_item(payload: &Value) -> Option<(Role, String)> {
    match string(payload, "type")? {
        "message" => {
            let role = match string(payload, "role")? {
                "user" if payload["isMeta"] != true => Role::User,
                "assistant" => Role::Assistant,
                _ => return None,
            };
            let record = json!({"type":"response_item","payload":payload});
            let text = message::parse_line(Agent::Codex, 0, &record)
                .into_iter()
                .filter(|msg| msg.kind == MessageKind::Text)
                .filter(|msg| {
                    matches!(
                        payload["content"][msg.block_index]["type"].as_str(),
                        Some("input_text" | "output_text")
                    )
                })
                .map(|msg| msg.text.trim().to_string())
                .filter(|text| {
                    !text.is_empty() && (role != Role::User || !message::is_meta_text(text))
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some((role, text))
        }
        "agent_message" => {
            if has_encrypted(payload, 0) {
                return None;
            }
            let text = match &payload["content"] {
                Value::String(text) => text.trim().to_string(),
                content => text_blocks(content).join("\n").trim().to_string(),
            };
            (!text.is_empty()).then_some((Role::Assistant, text))
        }
        "function_call" | "custom_tool_call" => {
            let name = string(payload, "name")
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown");
            let args = payload
                .get("arguments")
                .filter(|a| !a.is_null())
                .unwrap_or(&payload["input"]);
            let args = strip_encrypted(args, 0).unwrap_or(Value::Null);
            Some((
                Role::Assistant,
                format!("[codex tool call] {name}({})", stringify(&args)),
            ))
        }
        "function_call_output" | "custom_tool_call_output" => {
            let raw = &payload["output"];
            let value = strip_encrypted(raw, 0).unwrap_or(Value::Null);
            let text = empty_output(stringify(&value), raw);
            Some((Role::Assistant, format!("[codex tool result] {text}")))
        }
        _ => None,
    }
}

pub(super) fn ensure_roles(
    mut messages: Vec<FlatMessage>,
    source: Agent,
    source_id: &str,
) -> Vec<FlatMessage> {
    if messages.is_empty() {
        return messages;
    }
    if source == Agent::Codex {
        let mut merged: Vec<FlatMessage> = Vec::new();
        let mut parts = Vec::new();
        for message in messages {
            if merged.last().is_some_and(|last| last.role == message.role) {
                parts.push(message.text);
            } else {
                if let Some(last) = merged.last_mut() {
                    last.text = parts.join("\n\n");
                }
                parts.clear();
                parts.push(message.text.clone());
                merged.push(message);
            }
        }
        if let Some(last) = merged.last_mut() {
            last.text = parts.join("\n\n");
        }
        messages = merged;
    }
    if messages[0].role != Role::User {
        let product = if source == Agent::Claude {
            "Claude Code"
        } else {
            "Codex"
        };
        messages.insert(
            0,
            FlatMessage {
                role: Role::User,
                text: format!("[Transcript ported from {product} session {source_id}]"),
                timestamp: messages[0].timestamp.clone(),
            },
        );
    }
    if source == Agent::Codex
        && let Some(last) = messages.last().filter(|m| m.role == Role::User)
    {
        messages.push(FlatMessage {
            role: Role::Assistant,
            text: format!("[Transcript ported from Codex session {source_id} ended before this user message was answered.]"),
            timestamp: last.timestamp.clone(),
        });
    }
    messages
}
