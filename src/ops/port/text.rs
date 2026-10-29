//! Bounded tool-value rendering and encrypted-content removal.

use std::fmt::Write;

use serde_json::{Map, Value};

const TOOL_TEXT_CAP: usize = 1500;

pub(super) fn text_blocks(content: &Value) -> Vec<&str> {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| {
            matches!(
                block["type"].as_str(),
                Some("input_text" | "output_text" | "text")
            )
            .then(|| block["text"].as_str())
            .flatten()
            .filter(|s| !s.is_empty())
        })
        .collect()
}

pub(super) fn has_encrypted(value: &Value, depth: usize) -> bool {
    if depth > 100 {
        return true;
    }
    match value {
        Value::Object(map) => {
            map.contains_key("encrypted_content")
                || map.get("type").and_then(Value::as_str) == Some("encrypted_content")
                || map.values().any(|value| has_encrypted(value, depth + 1))
        }
        Value::Array(items) => items.iter().any(|value| has_encrypted(value, depth + 1)),
        _ => false,
    }
}

pub(super) fn strip_encrypted(value: &Value, depth: usize) -> Option<Value> {
    match value {
        Value::Object(map) => {
            if depth >= 100 || map.get("type").and_then(Value::as_str) == Some("encrypted_content")
            {
                return None;
            }
            Some(Value::Object(
                map.iter()
                    .filter(|(key, _)| key.as_str() != "encrypted_content")
                    .filter_map(|(key, value)| {
                        strip_encrypted(value, depth + 1).map(|value| (key.clone(), value))
                    })
                    .collect::<Map<_, _>>(),
            ))
        }
        Value::Array(items) => (depth < 100).then(|| {
            Value::Array(
                items
                    .iter()
                    .filter_map(|item| strip_encrypted(item, depth + 1))
                    .collect(),
            )
        }),
        _ => Some(value.clone()),
    }
}

#[derive(Default)]
struct Capped {
    text: String,
    total: usize,
}

impl Write for Capped {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let available = TOOL_TEXT_CAP.saturating_sub(self.total);
        self.text.extend(text.chars().take(available));
        self.total += text.chars().count();
        Ok(())
    }
}

impl Capped {
    fn finish(mut self) -> String {
        if self.total > TOOL_TEXT_CAP {
            self.text.push_str(&format!(
                "... [truncated {} chars]",
                self.total - TOOL_TEXT_CAP
            ));
        }
        self.text
    }
}

fn json_string(text: &str, output: &mut Capped) {
    let _ = output.write_char('"');
    for character in text.chars() {
        match character {
            '"' => {
                let _ = output.write_str("\\\"");
            }
            '\\' => {
                let _ = output.write_str("\\\\");
            }
            '\n' => {
                let _ = output.write_str("\\n");
            }
            '\r' => {
                let _ = output.write_str("\\r");
            }
            '\t' => {
                let _ = output.write_str("\\t");
            }
            '\u{08}' => {
                let _ = output.write_str("\\b");
            }
            '\u{0c}' => {
                let _ = output.write_str("\\f");
            }
            character if character < '\u{20}' => {
                let _ = write!(output, "\\u{:04x}", character as u32);
            }
            character => {
                let _ = output.write_char(character);
            }
        }
    }
    let _ = output.write_char('"');
}

fn json_value(value: &Value, output: &mut Capped) {
    match value {
        Value::String(text) => json_string(text, output),
        Value::Array(items) => {
            let _ = output.write_char('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    let _ = output.write_str(", ");
                }
                json_value(item, output);
            }
            let _ = output.write_char(']');
        }
        Value::Object(map) => {
            let _ = output.write_char('{');
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    let _ = output.write_str(", ");
                }
                json_string(key, output);
                let _ = output.write_str(": ");
                json_value(value, output);
            }
            let _ = output.write_char('}');
        }
        _ => {
            let _ = output.write_str(&value.to_string());
        }
    }
}

pub(super) fn stringify(value: &Value) -> String {
    let mut output = Capped::default();
    match value {
        Value::Null => {}
        Value::String(text) => {
            let _ = output.write_str(text.trim());
        }
        Value::Array(_) => {
            let parts = text_blocks(value);
            if parts.is_empty() {
                json_value(value, &mut output);
            } else {
                // Trim the logical joined text, without allocating an uncapped copy.
                let first = parts.iter().position(|part| !part.trim().is_empty());
                let last = parts.iter().rposition(|part| !part.trim().is_empty());
                if let (Some(first), Some(last)) = (first, last) {
                    for (index, part) in parts[first..=last].iter().enumerate() {
                        if index > 0 {
                            let _ = output.write_char('\n');
                        }
                        let part = if index == 0 { part.trim_start() } else { part };
                        let part = if index == last - first {
                            part.trim_end()
                        } else {
                            part
                        };
                        let _ = output.write_str(part);
                    }
                }
            }
        }
        _ => json_value(value, &mut output),
    }
    output.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cap_counts_characters_not_utf8_bytes() {
        assert_eq!(
            stringify(&json!(format!(" {} ", "\u{e9}".repeat(1502)))),
            format!("{}... [truncated 2 chars]", "\u{e9}".repeat(1500))
        );
        assert_eq!(
            stringify(&json!([{"type":"text","text":" a "},{"type":"text","text":" b "}])),
            "a \n b"
        );
        assert_eq!(
            stringify(&json!({"z":2,"a":"\u{e9}"})),
            "{\"a\": \"\u{e9}\", \"z\": 2}"
        );
    }

    #[test]
    fn encryption_is_removed_recursively() {
        let value = json!({"a":[{"type":"encrypted_content","text":"secret"},{"encrypted_content":null,"ok":1}],"encrypted_content":"secret"});
        assert!(has_encrypted(&value, 0));
        assert_eq!(strip_encrypted(&value, 0).unwrap(), json!({"a":[{"ok":1}]}));
    }
}
