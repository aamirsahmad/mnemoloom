//! Claude Code line processing for trim. Only changed lines are re-serialized.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use super::pystr::{py_str, py_str_len};
use super::{
    TrimOptions, TrimStats, already_trimmed, char_len, group, select_assistant, take_chars,
    tool_result_notice,
};
use crate::session::transcript::Line;

/// Per-line context for notices.
struct Ctx<'a> {
    opts: &'a TrimOptions,
    parent_file: &'a str,
    tool_map: &'a HashMap<String, String>,
}

/// Trim every line in place; returns the counters.
pub fn process(
    lines: &mut [Line],
    opts: &TrimOptions,
    parent_file: &str,
    new_session_id: Option<&str>,
) -> TrimStats {
    let tool_map = tool_names(lines);
    let selected: HashSet<usize> = opts
        .trim_assistant
        .map(|n| select_assistant(&assistant_candidates(lines, opts.threshold), n))
        .unwrap_or_default();
    let ctx = Ctx {
        opts,
        parent_file,
        tool_map: &tool_map,
    };
    let mut stats = TrimStats::default();
    for (i, line) in lines.iter_mut().enumerate() {
        let Some(Value::Object(orig)) = line.value() else {
            continue;
        };
        let mut data = orig.clone();
        let line_num = i + 1;
        let changed = process_line(
            &mut data,
            line_num,
            selected.contains(&line_num),
            &ctx,
            &mut stats,
        ) | replace_session_id(&mut data, new_session_id);
        if changed {
            line.set_value(Value::Object(data));
        }
    }
    stats
}

/// `tool_use.id -> name` over all assistant lines.
fn tool_names(lines: &[Line]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for v in lines.iter().filter_map(Line::value) {
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(Value::Array(items)) = v.pointer("/message/content") else {
            continue;
        };
        for item in items {
            if item.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let id = item.get("id").and_then(Value::as_str).unwrap_or("");
            let name = item.get("name").and_then(Value::as_str).unwrap_or("");
            if !id.is_empty() && !name.is_empty() {
                map.insert(id.to_string(), name.to_string());
            }
        }
    }
    map
}

/// 1-based line numbers of assistant messages whose untrimmed text is at
/// least `threshold` chars, in file order.
fn assistant_candidates(lines: &[Line], threshold: usize) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let v = l.value()?;
            if v.get("type").and_then(Value::as_str) != Some("assistant") {
                return None;
            }
            let Some(Value::Array(items)) = v.pointer("/message/content") else {
                return None;
            };
            let total: usize = items
                .iter()
                .filter(|it| it.get("type").and_then(Value::as_str) == Some("text"))
                .map(|it| it.get("text").cloned().unwrap_or(json!("")))
                .filter(|t| !already_trimmed(&py_str(t)))
                .map(|t| py_str_len(&t))
                .sum();
            (total >= threshold).then_some(i + 1)
        })
        .collect()
}

fn process_line(
    data: &mut Map<String, Value>,
    line_num: usize,
    selected: bool,
    ctx: &Ctx,
    stats: &mut TrimStats,
) -> bool {
    let mut modified = false;
    if data.get("error").and_then(Value::as_str) == Some("invalid_request") {
        data.insert("error".into(), Value::Null);
        modified = true;
    }
    if data.get("isApiErrorMessage") == Some(&Value::Bool(true)) {
        data.insert("isApiErrorMessage".into(), Value::Bool(false));
        modified = true;
    }
    let kind = data.get("type").and_then(Value::as_str).map(str::to_owned);
    let Some(Value::Object(msg)) = data.get_mut("message") else {
        return modified;
    };
    if msg.get("model").and_then(Value::as_str) == Some("<synthetic>") {
        msg.insert("model".into(), json!("trimmed"));
        modified = true;
        if msg.contains_key("content") {
            msg.insert(
                "content".into(),
                json!([{"type": "text", "text": "[Context limit reached in parent session - trimmed]"}]),
            );
        }
    }
    if let Some(Value::Object(usage)) = msg.get_mut("usage")
        && needs_zeroing(usage)
    {
        zero_usage(usage);
        modified = true;
    }
    match kind.as_deref() {
        Some("assistant") => modified |= trim_assistant(msg, line_num, selected, ctx, stats),
        Some("user") => modified |= trim_tool_results(msg, line_num, ctx, stats),
        _ => {}
    }
    modified
}

/// Numeric equality with the zeroed dict (so `0.0`/`false` count as 0).
fn needs_zeroing(usage: &Map<String, Value>) -> bool {
    usage.values().any(|v| match v {
        Value::Object(m) => needs_zeroing(m),
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        _ => false,
    })
}

/// Zero every int/float/bool (Python `isinstance(v, (int, float))`), recursively.
fn zero_usage(usage: &mut Map<String, Value>) {
    for v in usage.values_mut() {
        match v {
            Value::Object(m) => zero_usage(m),
            Value::Bool(_) | Value::Number(_) => *v = json!(0),
            _ => {}
        }
    }
}

fn trim_assistant(
    msg: &mut Map<String, Value>,
    line_num: usize,
    selected: bool,
    ctx: &Ctx,
    stats: &mut TrimStats,
) -> bool {
    let Some(Value::Array(items)) = msg.get_mut("content") else {
        return false;
    };
    let threshold = ctx.opts.threshold;
    let mut modified = false;
    for item in items.iter_mut().filter_map(Value::as_object_mut) {
        match item.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                let Some(Value::Object(input)) = item.get_mut("input") else {
                    continue;
                };
                for (k, v) in input.iter_mut() {
                    let Value::String(s) = v else { continue };
                    let len = char_len(s);
                    if len < threshold || already_trimmed(s) {
                        continue;
                    }
                    let new = format!(
                        "{}\n\n[...truncated {k} - was {} chars. See line {line_num} of {}]",
                        take_chars(s, threshold),
                        group(len as i64),
                        ctx.parent_file
                    );
                    let saved = len as i64 - char_len(&new) as i64;
                    *s = new;
                    modified = true;
                    if saved > 0 {
                        stats.chars_saved += saved;
                        stats.num_tools_trimmed += 1;
                    }
                }
            }
            Some("text") if selected => {
                let Some(Value::String(text)) = item.get_mut("text") else {
                    continue;
                };
                let len = char_len(text);
                if already_trimmed(text) || len < threshold {
                    continue;
                }
                let placeholder = format!(
                    "[Assistant message trimmed - original content was {} characters. See line {line_num} of {} for full content]",
                    group(len as i64),
                    ctx.parent_file
                );
                stats.chars_saved += len as i64 - char_len(&placeholder) as i64;
                stats.num_assistant_trimmed += 1;
                *text = placeholder;
                modified = true;
            }
            _ => {}
        }
    }
    modified
}

fn trim_tool_results(
    msg: &mut Map<String, Value>,
    line_num: usize,
    ctx: &Ctx,
    stats: &mut TrimStats,
) -> bool {
    let Some(Value::Array(items)) = msg.get_mut("content") else {
        return false;
    };
    let threshold = ctx.opts.threshold;
    let mut modified = false;
    for item in items.iter_mut().filter_map(Value::as_object_mut) {
        if item.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let name = item
            .get("tool_use_id")
            .and_then(Value::as_str)
            .and_then(|id| ctx.tool_map.get(id))
            .map_or("Unknown", String::as_str);
        if !ctx.opts.tool_selected(name) {
            continue;
        }
        let content = item.get("content").cloned().unwrap_or(json!(""));
        let len = content_length(&content);
        if len < threshold {
            continue;
        }
        let truncated = truncate_content(&content, threshold, line_num, ctx.parent_file);
        let saved = len as i64 - char_len(&truncated) as i64;
        if saved > 0 {
            item.insert("content".into(), Value::String(truncated));
            modified = true;
            stats.num_tools_trimmed += 1;
            stats.chars_saved += saved;
        }
    }
    modified
}

/// Character length of a content field (non-strings measured flattened).
fn content_length(content: &Value) -> usize {
    match content {
        Value::String(s) => char_len(s),
        Value::Array(items) => items
            .iter()
            .map(|it| match it.get("text") {
                Some(Value::String(t)) if it.is_object() => char_len(t),
                _ => py_str_len(it),
            })
            .sum(),
        other => py_str_len(other),
    }
}

/// Flatten to a string, then truncate unless
/// already trimmed, short, or not actually shorter.
fn truncate_content(content: &Value, threshold: usize, line_num: usize, parent: &str) -> String {
    let flat = match content {
        Value::Array(items) => items
            .iter()
            .map(|it| match it.get("text") {
                Some(Value::String(t)) if it.is_object() => t.clone(),
                _ => py_str(it),
            })
            .collect(),
        other => py_str(other),
    };
    let len = char_len(&flat);
    if already_trimmed(&flat) || len <= threshold {
        return flat;
    }
    let result = format!(
        "{}{}",
        take_chars(&flat, threshold),
        tool_result_notice(len, threshold, line_num, parent)
    );
    if char_len(&result) >= len {
        flat
    } else {
        result
    }
}

fn replace_session_id(data: &mut Map<String, Value>, new_id: Option<&str>) -> bool {
    match (new_id, data.get("sessionId")) {
        (Some(new), Some(old)) if old.as_str() != Some(new) => {
            data.insert("sessionId".into(), json!(new));
            true
        }
        _ => false,
    }
}
