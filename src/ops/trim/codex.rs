//! Codex rollout line processing for trim. Only changed lines are
//! re-serialized (untouched lines stay byte-identical).

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use super::pystr::{py_str, py_str_len};
use super::{
    TrimOptions, TrimStats, char_len, group, select_assistant, take_chars, tool_result_notice,
};
use crate::session::pyjson;
use crate::session::transcript::Line;

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
    let mut stats = TrimStats::default();
    for (i, line) in lines.iter_mut().enumerate() {
        let Some(Value::Object(orig)) = line.value() else {
            continue;
        };
        let mut data = orig.clone();
        let line_num = i + 1;
        let mut changed = replace_session_id(&mut data, new_session_id);
        if selected.contains(&line_num) {
            changed |= trim_assistant(&mut data, line_num, opts.threshold, parent_file, &mut stats);
        }
        changed |= trim_output(
            &mut data,
            line_num,
            opts,
            parent_file,
            &tool_map,
            &mut stats,
        );
        if changed {
            line.set_value(Value::Object(data));
        }
    }
    stats
}

fn response_payload(v: &Value, payload_type: &str) -> bool {
    v.get("type").and_then(Value::as_str) == Some("response_item")
        && v.pointer("/payload/type").and_then(Value::as_str) == Some(payload_type)
}

/// `function_call.call_id -> name`.
fn tool_names(lines: &[Line]) -> HashMap<String, String> {
    lines
        .iter()
        .filter_map(Line::value)
        .filter(|v| response_payload(v, "function_call"))
        .filter_map(|v| {
            let id = v.pointer("/payload/call_id")?.as_str()?;
            let name = v.pointer("/payload/name")?.as_str()?;
            (!id.is_empty() && !name.is_empty()).then(|| (id.to_string(), name.to_string()))
        })
        .collect()
}

fn is_assistant_message(v: &Value) -> bool {
    response_payload(v, "message")
        && v.pointer("/payload/role").and_then(Value::as_str) == Some("assistant")
}

fn assistant_candidates(lines: &[Line], threshold: usize) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let v = l.value().filter(|v| is_assistant_message(v))?;
            let total: usize = v
                .pointer("/payload/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|it| it.get("type").and_then(Value::as_str) == Some("output_text"))
                .map(|it| py_str_len(it.get("text").unwrap_or(&json!(""))))
                .sum();
            (total >= threshold).then_some(i + 1)
        })
        .collect()
}

fn replace_session_id(data: &mut Map<String, Value>, new_id: Option<&str>) -> bool {
    let Some(new) = new_id else { return false };
    if data.get("type").and_then(Value::as_str) != Some("session_meta") {
        return false;
    }
    match data.get_mut("payload") {
        Some(Value::Object(p)) if p.contains_key("id") => {
            p.insert("id".into(), json!(new));
            true
        }
        _ => false,
    }
}

fn trim_assistant(
    data: &mut Map<String, Value>,
    line_num: usize,
    threshold: usize,
    parent_file: &str,
    stats: &mut TrimStats,
) -> bool {
    let mut modified = false;
    let Some(Value::Object(payload)) = data.get_mut("payload") else {
        return false;
    };
    if payload.get("type").and_then(Value::as_str) != Some("message")
        || payload.get("role").and_then(Value::as_str) != Some("assistant")
    {
        return false;
    }
    let Some(Value::Array(items)) = payload.get_mut("content") else {
        return false;
    };
    for item in items.iter_mut().filter_map(Value::as_object_mut) {
        if item.get("type").and_then(Value::as_str) != Some("output_text") {
            continue;
        }
        let Some(Value::String(text)) = item.get_mut("text") else {
            continue;
        };
        let len = char_len(text);
        if len < threshold {
            continue;
        }
        let placeholder = format!(
            "[Assistant message trimmed - original content was {} characters. See line {line_num} of {parent_file} for full content]",
            group(len as i64)
        );
        stats.chars_saved += len as i64 - char_len(&placeholder) as i64;
        stats.num_assistant_trimmed += 1;
        *text = placeholder;
        modified = true;
    }
    modified
}

fn trim_output(
    data: &mut Map<String, Value>,
    line_num: usize,
    opts: &TrimOptions,
    parent_file: &str,
    tool_map: &HashMap<String, String>,
    stats: &mut TrimStats,
) -> bool {
    if data.get("type").and_then(Value::as_str) != Some("response_item") {
        return false;
    }
    let Some(Value::Object(payload)) = data.get_mut("payload") else {
        return false;
    };
    if payload.get("type").and_then(Value::as_str) != Some("function_call_output") {
        return false;
    }
    let name = payload
        .get("call_id")
        .and_then(Value::as_str)
        .and_then(|id| tool_map.get(id))
        .map_or("Unknown", String::as_str);
    let Some(Value::String(output)) = payload.get("output") else {
        return false;
    };
    let parsed: Option<Value> = serde_json::from_str(output).ok();
    let content = match &parsed {
        Some(Value::Object(o)) if o.contains_key("output") => py_str(&o["output"]),
        Some(other) => py_str(other),
        None => output.clone(),
    };
    let len = char_len(&content);
    // `len >= threshold` selects, but `len <= threshold` is kept as is.
    if len <= opts.threshold || !opts.tool_selected(name) {
        return false;
    }
    let metadata = match &parsed {
        Some(Value::Object(o)) => o.get("metadata").cloned().unwrap_or(json!({})),
        _ => json!({}),
    };
    let text = format!(
        "{}{}",
        take_chars(&content, opts.threshold),
        tool_result_notice(len, opts.threshold, line_num, parent_file)
    );
    let truncated = pyjson::dumps(&json!({"output": text, "metadata": metadata}));
    let saved = char_len(output) as i64 - char_len(&truncated) as i64;
    if saved <= 0 {
        return false;
    }
    payload.insert("output".into(), Value::String(truncated));
    stats.num_tools_trimmed += 1;
    stats.chars_saved += saved;
    true
}
