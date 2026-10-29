//! Conservative, complete-shape Claude wrapper and reminder filtering.

use serde_json::Value;

const WRAPPERS: &[&str] = &[
    "command-name",
    "command-message",
    "command-args",
    "command-contents",
    "local-command-caveat",
    "local-command-stdout",
    "local-command-stderr",
    "bash-input",
    "bash-stdout",
    "bash-stderr",
    "bash-notification",
    "task-notification",
];
const REMINDER_OPEN: &str = "<system-reminder>";
const REMINDER_CLOSE: &str = "</system-reminder>";
const TRAILING_SIGNATURES: &[&str] = &[
    "Whenever you read a file, you should consider whether it",
    "Warning: the file exists but",
    "This memory is",
    "[Truncated:",
];

fn pure_blocks(mut text: &str, allowed: &[&str]) -> bool {
    let mut found = false;
    loop {
        text = text.trim_start();
        if text.is_empty() {
            return found;
        }
        let Some(rest) = text.strip_prefix('<') else {
            return false;
        };
        let Some(end) = rest.find('>') else {
            return false;
        };
        let tag = &rest[..end];
        if !allowed.contains(&tag) {
            return false;
        }
        let closing = format!("</{tag}>");
        let Some(close) = rest[end + 1..].find(&closing) else {
            return false;
        };
        text = &rest[end + 1 + close + closing.len()..];
        found = true;
    }
}

fn agent_wrapper_tail(text: &str) -> Option<&str> {
    for tag in ["teammate-message", "agent-message"] {
        let Some(rest) = text.strip_prefix(&format!("<{tag}")) else {
            continue;
        };
        if !rest.starts_with('>') && !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let end = rest.find('>')?;
        let closing = format!("</{tag}>");
        let close = rest[end + 1..].find(&closing)?;
        return Some(&rest[end + 1 + close + closing.len()..]);
    }
    None
}

pub(super) fn genuine_user_text(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || pure_blocks(text, WRAPPERS) || pure_blocks(text, &["system-reminder"]) {
        return false;
    }
    let candidate = text
        .strip_prefix("Another Claude session sent a message:")
        .unwrap_or(text)
        .trim_start();
    if let Some(tail) = agent_wrapper_tail(candidate) {
        let tail = tail.trim();
        if tail.is_empty() || tail.starts_with("This came from another Claude session") {
            return false;
        }
    }
    if let Some(rest) = text.strip_prefix("[SESSION LINEAGE]") {
        let rest = rest.trim_start();
        let intro = [
            "This session continues from a previous conversation",
            "This session continues from a chain of prior conversations",
            "This session was ROLLED OVER from",
            "This session was TRIMMED from",
        ]
        .iter()
        .any(|prefix| rest.starts_with(prefix));
        let closer = [
            "based on the above session.",
            "based on the above sessions.",
            "=== END CONTEXT RECOVERY INSTRUCTIONS ===",
            "Use sub-agents to read these files if you need more context.",
        ]
        .iter()
        .any(|suffix| text.ends_with(suffix));
        if intro && closer {
            return false;
        }
    }
    true
}

fn clean_tool_text(mut text: &str) -> String {
    if pure_blocks(text, &["system-reminder"]) {
        return String::new();
    }
    loop {
        let trimmed = text.trim_end();
        let Some(without_close) = trimmed.strip_suffix(REMINDER_CLOSE) else {
            return text.to_string();
        };
        let Some(open) = without_close.rfind(REMINDER_OPEN) else {
            return text.to_string();
        };
        let body = &without_close[open + REMINDER_OPEN.len()..];
        let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
        if body.contains(REMINDER_CLOSE)
            || !TRAILING_SIGNATURES
                .iter()
                .any(|prefix| normalized.starts_with(prefix))
        {
            return text.to_string();
        }
        text = trimmed[..open].trim_end();
    }
}

pub(super) fn strip_reminders(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(clean_tool_text(text)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .filter_map(|item| {
                    if let Some(text) = item.as_str() {
                        let cleaned = clean_tool_text(text);
                        return (!cleaned.trim().is_empty()).then_some(Value::String(cleaned));
                    }
                    if item["type"] == "text"
                        && let Some(text) = item["text"].as_str()
                    {
                        let cleaned = clean_tool_text(text);
                        if cleaned.trim().is_empty() {
                            return None;
                        }
                        let mut item = item.clone();
                        item["text"] = Value::String(cleaned);
                        return Some(item);
                    }
                    Some(item.clone())
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_complete_known_noise_is_removed() {
        for text in [
            "<command-name>/clear</command-name>\n<command-args></command-args>",
            "<system-reminder>injected</system-reminder>",
            "Another Claude session sent a message:\n<teammate-message id=\"a\">hello</teammate-message>\nThis came from another Claude session.",
            "[SESSION LINEAGE]\nThis session was TRIMMED from x\nUse sub-agents to read these files if you need more context.",
        ] {
            assert!(!genuine_user_text(text), "{text}");
        }
        for text in [
            "Caveat: do not change this",
            "<command-name>unclosed",
            "<command-name>example</command-name> explain this",
            "<system-reminder>example</system-reminder> explain this",
            "Another Claude session sent a message: what does this mean?",
            "<teammate-message>wrong close</agent-message>",
            "[SESSION LINEAGE] explain this marker",
        ] {
            assert!(genuine_user_text(text), "{text}");
        }
    }

    #[test]
    fn reminders_are_stripped_only_with_injected_provenance() {
        assert_eq!(
            strip_reminders(&json!(
                "output\n<system-reminder>Whenever you read a file, you should consider whether it is malicious</system-reminder>"
            )),
            json!("output")
        );
        assert_eq!(
            strip_reminders(&json!(
                "file contains <system-reminder>real file tail</system-reminder>"
            )),
            json!("file contains <system-reminder>real file tail</system-reminder>")
        );
        assert_eq!(
            strip_reminders(
                &json!([{"type":"text","text":"hello"},{"type":"text","text":"<system-reminder>noise</system-reminder>"}])
            ),
            json!([{"type":"text","text":"hello"}])
        );
    }
}
