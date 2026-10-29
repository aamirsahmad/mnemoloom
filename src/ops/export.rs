//! Plain-text transcript exports in Claude Code's built-in transcript style.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use serde_json::Value;

use crate::cli::{ExportAgentArgs, ExportArgs, GlobalArgs, agent_of};
use crate::session::launch::prompt_line;
use crate::session::transcript::write_atomic;
use crate::session::{
    self, Agent, Message, MessageKind, Resolution, ResolveOptions, Role, Transcript, pyjson,
};

const ASSISTANT: &str = "\u{23fa} ";
const RESULT: &str = "  \u{23bf}  ";

#[derive(Debug, Default, PartialEq, Eq)]
struct Stats {
    user_messages: usize,
    assistant_messages: usize,
    tool_calls: usize,
    tool_results: usize,
    skipped: usize,
}

/// Render an already loaded transcript without writing files or printing progress.
pub fn render_markdown(transcript: &Transcript) -> anyhow::Result<String> {
    Ok(render(transcript, false).0)
}

/// `mnemoloom export [SESSION] [--agent] [-o PATH] [-v]`.
pub fn export(global: &GlobalArgs, args: &ExportArgs) -> anyhow::Result<ExitCode> {
    run(
        global,
        args.session.as_deref(),
        agent_of(args.agent),
        args.output.as_deref(),
        args.verbose,
        true,
    )
}

/// `mnemoloom export-claude [SESSION] [-o PATH] [-v]`.
pub fn export_claude(global: &GlobalArgs, args: &ExportAgentArgs) -> anyhow::Result<ExitCode> {
    run(
        global,
        args.session.as_deref(),
        Some(Agent::Claude),
        args.output.as_deref(),
        args.verbose,
        false,
    )
}

/// `mnemoloom export-codex [SESSION] [-o PATH] [-v]`.
pub fn export_codex(global: &GlobalArgs, args: &ExportAgentArgs) -> anyhow::Result<ExitCode> {
    run(
        global,
        args.session.as_deref(),
        Some(Agent::Codex),
        args.output.as_deref(),
        args.verbose,
        false,
    )
}

fn run(
    global: &GlobalArgs,
    query: Option<&str>,
    agent: Option<Agent>,
    output: Option<&Path>,
    verbose: bool,
    announce_agent: bool,
) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    let resolved = match query {
        Some(query) => session::resolve(
            query,
            &ResolveOptions::new(&homes)
                .agent(agent)
                .prompt("Which session do you want to export?"),
        )?,
        None => {
            crate::commands::latest_session(&homes, agent, "Which session do you want to export?")?
        }
    };
    if announce_agent && query.is_some() {
        let label = resolved.agent.as_str().to_uppercase();
        println!("\nDetected {label} session\nExporting with {label}\n");
    }
    let interactive = query.is_none();
    let path = export_path(&resolved, output, interactive)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if verbose {
        println!(
            "\u{1f4c4} Exporting session: {}",
            crate::commands::file_name(&resolved.session_file)
        );
        println!("\u{1f4dd} Output file: {}\n", path.display());
    }
    let transcript = Transcript::load(&resolved.session_file, resolved.agent)?;
    let (text, stats) = render(&transcript, verbose);
    write_atomic(&path, text.as_bytes(), None)
        .with_context(|| format!("writing {}", path.display()))?;
    if interactive {
        println!();
    }
    print_stats(&stats);
    let label = if interactive { "Exported to" } else { "Output" };
    println!("\n\u{1f4c4} {label}: {}", path.display());
    Ok(ExitCode::SUCCESS)
}

fn export_path(
    resolved: &Resolution,
    output: Option<&Path>,
    interactive: bool,
) -> anyhow::Result<PathBuf> {
    if !interactive {
        return Ok(output.map(Path::to_path_buf).unwrap_or_else(|| {
            session::default_export_path(
                &resolved.session_file,
                resolved.agent,
                resolved.cwd.as_deref(),
            )
        }));
    }
    let cwd = resolved
        .cwd
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let default = cwd.join("exported-sessions").join(format!(
        "{}-{}-session-{}.txt",
        chrono::Local::now().format("%Y%m%d"),
        resolved.agent,
        resolved.session_id()
    ));
    println!("\nDefault export path: {}", default.display());
    let mut path = match output {
        Some(path) => session::homes::expand_tilde(path),
        None => {
            let response = prompt_line("Path (or Enter for default): ")?.unwrap_or_default();
            if response.is_empty() {
                default
            } else {
                session::homes::expand_tilde(Path::new(&response))
            }
        }
    };
    if resolved.agent == Agent::Claude || output.is_some() {
        path.set_extension("txt");
    }
    Ok(path)
}

fn print_stats(stats: &Stats) {
    println!("\u{2705} Export complete!");
    println!("   User messages: {}", stats.user_messages);
    println!("   Assistant messages: {}", stats.assistant_messages);
    println!("   Tool calls: {}", stats.tool_calls);
    println!("   Tool results: {}", stats.tool_results);
    println!("   Skipped items: {}", stats.skipped);
}

fn render(transcript: &Transcript, verbose: bool) -> (String, Stats) {
    if !transcript.agent.is_rewritable() {
        return render_messages(transcript);
    }
    let messages = transcript.messages();
    let messages: HashMap<(usize, usize), &Message> = messages
        .iter()
        .map(|message| ((message.line_index, message.block_index), message))
        .collect();
    let mut output = String::new();
    let mut stats = Stats::default();
    for (index, line) in transcript.lines.iter().enumerate() {
        let Some(value) = line.value() else {
            if !line.raw().iter().all(u8::is_ascii_whitespace) {
                stats.skipped += 1;
                if verbose {
                    eprintln!(
                        "\u{26a0}\u{fe0f}  Line {}: Invalid JSON, skipping",
                        index + 1
                    );
                }
            }
            continue;
        };
        match transcript.agent {
            Agent::Claude => render_claude(value, index, &messages, &mut output, &mut stats),
            Agent::Codex => render_codex(value, index, &messages, &mut output, &mut stats),
            Agent::Pi | Agent::OpenCode => {}
        }
    }
    (output, stats)
}

/// pi / opencode: rendered from the common message model (pi: the active
/// branch only). Meta text and non-conversation blocks count as skipped.
fn render_messages(transcript: &Transcript) -> (String, Stats) {
    let mut output = String::new();
    let mut stats = Stats::default();
    for m in transcript.display_messages() {
        match (m.kind, m.role) {
            (MessageKind::Text, Role::User | Role::Assistant)
                if !m.is_meta && !m.text.trim().is_empty() =>
            {
                render_text(&m.text, m.role.as_str(), &mut output, &mut stats);
            }
            (MessageKind::ToolUse, _) => {
                let input = m.tool_input.unwrap_or(Value::Null);
                render_call(
                    m.tool_name.as_deref().unwrap_or("Unknown"),
                    &input,
                    &mut output,
                );
                stats.tool_calls += 1;
            }
            (MessageKind::ToolResult, _) => {
                render_result(&m.text, &mut output);
                stats.tool_results += 1;
            }
            _ => stats.skipped += 1,
        }
    }
    (output, stats)
}

type Messages<'a> = HashMap<(usize, usize), &'a Message>;

fn render_claude(
    value: &Value,
    index: usize,
    messages: &Messages<'_>,
    output: &mut String,
    stats: &mut Stats,
) {
    if !matches!(value["type"].as_str(), Some("user" | "assistant")) {
        stats.skipped += 1;
        return;
    }
    let role = value["message"]["role"].as_str().unwrap_or("");
    let content = &value["message"]["content"];
    match content {
        Value::String(text) if !text.is_empty() => render_text(text, role, output, stats),
        Value::Array(blocks) if !blocks.is_empty() => {
            for (bi, block) in blocks.iter().enumerate() {
                let parsed = messages.get(&(index, bi));
                if let Some(text) = block.as_str() {
                    render_text(text, role, output, stats);
                    continue;
                }
                match (role, block["type"].as_str()) {
                    ("user" | "assistant", Some("text")) => {
                        let text = parsed.map_or("", |m| m.text.as_str());
                        if !text.trim().is_empty() {
                            render_text(text, role, output, stats);
                        }
                    }
                    ("assistant", Some("tool_use")) => {
                        render_call(
                            block["name"].as_str().unwrap_or("Unknown"),
                            &block["input"],
                            output,
                        );
                        stats.tool_calls += 1;
                    }
                    ("user", Some("tool_result")) => {
                        let text = match &block["content"] {
                            Value::Array(items) => items
                                .iter()
                                .filter(|item| item["type"] == "text")
                                .filter_map(|item| item["text"].as_str())
                                .collect::<String>(),
                            Value::Null if block.get("content").is_none() => String::new(),
                            other => python_str(other),
                        };
                        render_result(&text, output);
                        stats.tool_results += 1;
                    }
                    _ => stats.skipped += 1,
                }
            }
        }
        _ => stats.skipped += 1,
    }
}

fn render_codex(
    value: &Value,
    index: usize,
    messages: &Messages<'_>,
    output: &mut String,
    stats: &mut Stats,
) {
    if value["type"] != "response_item" {
        stats.skipped += 1;
        return;
    }
    let payload = &value["payload"];
    match payload["type"].as_str() {
        Some("message") => {
            let Some(blocks) = payload["content"].as_array() else {
                if payload.get("content").is_some() {
                    stats.skipped += 1;
                }
                return;
            };
            for (bi, block) in blocks.iter().enumerate() {
                let role = payload["role"].as_str().unwrap_or("");
                if matches!(
                    (role, block["type"].as_str()),
                    ("user", Some("input_text")) | ("assistant", Some("output_text"))
                ) && let Some(message) = messages.get(&(index, bi))
                    && !message.text.trim().is_empty()
                {
                    render_text(&message.text, role, output, stats);
                }
            }
        }
        Some("function_call" | "custom_tool_call") => {
            let input = if payload["type"] == "function_call" {
                &payload["arguments"]
            } else {
                &payload["input"]
            };
            let parsed;
            let input = if let Some(text) = input.as_str() {
                parsed = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
                &parsed
            } else {
                input
            };
            render_call(payload["name"].as_str().unwrap_or("Unknown"), input, output);
            stats.tool_calls += 1;
        }
        Some("function_call_output" | "custom_tool_call_output") => {
            let mut result = payload.get("output").cloned().unwrap_or(Value::Null);
            if let Some(text) = result.as_str()
                && let Ok(parsed) = serde_json::from_str::<Value>(text)
                && let Some(inner) = parsed.get("output")
            {
                result = inner.clone();
            }
            let text = if truthy(&result) {
                python_str(&result)
            } else {
                String::new()
            };
            render_result(&text, output);
            stats.tool_results += 1;
        }
        _ => stats.skipped += 1,
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Number(value) => value.as_f64() != Some(0.0),
    }
}

fn python_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(python_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(items) => format!(
            "{{{}}}",
            items
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    python_repr(&Value::String(key.clone())),
                    python_repr(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Number(number) => number.to_string(),
    }
}

fn python_repr(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        let quote = if text.contains('\'') && !text.contains('"') {
            '"'
        } else {
            '\''
        };
        let text = text
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
            .replace(quote, &format!("\\{quote}"));
        format!("{quote}{text}{quote}")
    } else {
        python_str(value)
    }
}

fn render_text(text: &str, role: &str, output: &mut String, stats: &mut Stats) {
    let prefix = match role {
        "user" => {
            stats.user_messages += 1;
            "> "
        }
        "assistant" => {
            stats.assistant_messages += 1;
            ASSISTANT
        }
        _ => return,
    };
    for paragraph in text.trim().split("\n\n") {
        if !paragraph.trim().is_empty() {
            output.push_str(&wrap(paragraph, prefix));
            output.push_str("\n\n");
        }
    }
}

// Python textwrap preserves internal whitespace runs, expands tabs, and
// replaces physical newlines without breaking long or hyphenated words.
fn wrap(text: &str, prefix: &str) -> String {
    let mut expanded = String::new();
    let mut column = 0;
    for character in text.chars() {
        if character == '\t' {
            let spaces = 8 - column % 8;
            expanded.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            expanded.push(if matches!(character, '\n' | '\r' | '\x0b' | '\x0c') {
                ' '
            } else {
                character
            });
            column = if matches!(character, '\n' | '\r') {
                0
            } else {
                column + 1
            };
        }
    }
    let mut chunks: VecDeque<&str> = VecDeque::new();
    let mut start = 0;
    let mut whitespace = None;
    for (offset, character) in expanded.char_indices() {
        let current = character == ' ';
        if whitespace.is_some_and(|previous| previous != current) {
            chunks.push_back(&expanded[start..offset]);
            start = offset;
        }
        whitespace = Some(current);
    }
    if start < expanded.len() {
        chunks.push_back(&expanded[start..]);
    }
    let mut lines = Vec::new();
    while !chunks.is_empty() {
        if !lines.is_empty() && chunks.front().is_some_and(|s| s.trim().is_empty()) {
            chunks.pop_front();
        }
        let mut parts = Vec::new();
        let mut length = 0;
        while let Some(chunk) = chunks.front() {
            let size = chunk.chars().count();
            if length + size > 98 {
                break;
            }
            length += size;
            parts.push(chunks.pop_front().unwrap_or_default());
        }
        if parts.is_empty()
            && let Some(chunk) = chunks.pop_front()
        {
            parts.push(chunk);
        }
        if parts.last().is_some_and(|s| s.trim().is_empty()) {
            parts.pop();
        }
        if !parts.is_empty() {
            let indent = if lines.is_empty() { prefix } else { "  " };
            lines.push(format!("{indent}{}", parts.concat()));
        }
    }
    lines.join("\n")
}

fn render_call(name: &str, input: &Value, output: &mut String) {
    output.push_str(&format!(
        "{ASSISTANT}{name}({})\n\n",
        simplify_tool_args(input)
    ));
}

fn simplify_tool_args(input: &Value) -> String {
    let Some(args) = input.as_object() else {
        return String::new();
    };
    if args.len() == 1
        && let Some((key, Value::String(value))) = args.iter().next()
        && [
            "command",
            "file_path",
            "pattern",
            "path",
            "query",
            "url",
            "prompt",
        ]
        .contains(&key.as_str())
        && value.chars().count() < 100
    {
        return value.clone();
    }
    args.iter()
        .map(|(key, value)| {
            let rendered = match value {
                Value::String(text) if text.contains([' ', ',', '(', ')']) => format!("\"{text}\""),
                Value::String(text) => text.clone(),
                other => pyjson::dumps(other),
            };
            format!("{key}={rendered}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_result(text: &str, output: &mut String) {
    output.push_str(RESULT);
    if text.is_empty() {
        output.push_str("(No content)");
    } else {
        output.push_str(&text.replace('\n', "\n     "));
    }
    output.push_str("\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;
    use serde_json::json;

    fn fixture(agent: Agent, values: &[Value]) -> (tempfile::TempDir, Transcript) {
        let dir = tempfile::tempdir().unwrap();
        let path = write_raw(
            &dir.path().join("session.jsonl"),
            &values.iter().map(Value::to_string).collect::<Vec<_>>(),
        );
        let transcript = Transcript::load(&path, agent).unwrap();
        (dir, transcript)
    }

    #[test]
    fn claude_fixture_and_stats() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_claude(
            dir.path(),
            CLAUDE_ID,
            "/work",
            &claude_lines(CLAUDE_ID, "/work"),
        );
        let transcript = Transcript::load(&path, Agent::Claude).unwrap();
        let (text, stats) = render(&transcript, false);
        assert_eq!(
            text,
            format!(
                "> <command-name>/clear</command-name>\n\n> fix the failing spec in user_spec.rb\n\n{ASSISTANT}Bash(command=\"bundle exec rspec\", description=\"Run spec\")\n\n{RESULT}F.\n     Failures:\n\n{ASSISTANT}Fixed.\n\n"
            )
        );
        assert_eq!(
            stats,
            Stats {
                user_messages: 2,
                assistant_messages: 1,
                tool_calls: 1,
                tool_results: 1,
                skipped: 5
            }
        );
        assert_eq!(render_markdown(&transcript).unwrap(), text);
    }

    #[test]
    fn codex_fixture_and_stats() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_codex(
            dir.path(),
            CODEX_ID,
            "2026-05-07T12-57-30",
            &codex_lines(CODEX_ID, "/work", "cli"),
        );
        let transcript = Transcript::load(&path, Agent::Codex).unwrap();
        let (text, stats) = render(&transcript, false);
        assert!(text.contains(&format!(
            "{ASSISTANT}exec_command(cmd=ls)\n\n{RESULT}a\n     b\n\n"
        )));
        assert_eq!(
            stats,
            Stats {
                user_messages: 2,
                assistant_messages: 1,
                tool_calls: 1,
                tool_results: 1,
                skipped: 3
            }
        );
    }

    #[test]
    fn paragraphs_whitespace_and_unbroken_words() {
        assert_eq!(
            wrap(&format!("{} next", "x".repeat(98)), "> "),
            format!("> {}\n  next", "x".repeat(98))
        );
        assert_eq!(
            wrap(&format!("{} end", "long-".repeat(30)), ASSISTANT),
            format!("{ASSISTANT}{}\n  end", "long-".repeat(30))
        );
        assert_eq!(
            wrap("one   two\nthree\tfour", "> "),
            "> one   two three   four"
        );
        let (_dir, transcript) = fixture(
            Agent::Claude,
            &[json!({"type":"user","message":{"role":"user","content":" first\n\n second "}})],
        );
        assert_eq!(
            render_markdown(&transcript).unwrap(),
            "> first\n\n>  second\n\n"
        );
    }

    #[test]
    fn tool_argument_rules() {
        assert_eq!(
            simplify_tool_args(&json!({"command":"echo hello"})),
            "echo hello"
        );
        assert_eq!(
            simplify_tool_args(&json!({"path":"x".repeat(100)})),
            format!("path={}", "x".repeat(100))
        );
        assert_eq!(
            simplify_tool_args(&json!({"command":"x(y)","ok":true,"n":2,"nested":["\u{e9}",null]})),
            r#"command="x(y)", ok=true, n=2, nested=["\u00e9", null]"#
        );
        assert_eq!(simplify_tool_args(&json!({})), "");
    }

    #[test]
    fn results_concat_blocks_and_custom_calls() {
        let (_dir, transcript) = fixture(
            Agent::Claude,
            &[
                json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]},{"type":"tool_result","content":""},{"type":"image"}]}}),
                json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hidden"},{"type":"text","text":"visible"}]}}),
            ],
        );
        assert_eq!(
            render_markdown(&transcript).unwrap(),
            format!("{RESULT}ab\n\n{RESULT}(No content)\n\n{ASSISTANT}visible\n\n")
        );
        let (_dir, transcript) = fixture(
            Agent::Codex,
            &[
                json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"patch","input":"not json"}}),
                json!({"type":"response_item","payload":{"type":"custom_tool_call_output","output":"{\"output\":\"done\\nnext\"}"}}),
            ],
        );
        assert_eq!(
            render_markdown(&transcript).unwrap(),
            format!("{ASSISTANT}patch()\n\n{RESULT}done\n     next\n\n")
        );
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let transcript = Transcript::from_bytes(
            Path::new("/fixture"),
            Agent::Claude,
            b"\nnot json\nnull\n[]\n",
        );
        assert_eq!(
            render(&transcript, false),
            (
                String::new(),
                Stats {
                    skipped: 3,
                    ..Stats::default()
                }
            )
        );
    }

    #[test]
    fn explicit_and_default_paths() {
        let resolved = Resolution {
            agent: Agent::Claude,
            session_file: PathBuf::from("/home/source.jsonl"),
            cwd: Some("/work/project".into()),
            git_branch: None,
        };
        assert_eq!(
            export_path(&resolved, None, false).unwrap(),
            Path::new("/work/project/exported-sessions/claude/source.txt")
        );
        assert_eq!(
            export_path(&resolved, Some(Path::new("out.md")), false).unwrap(),
            Path::new("out.md")
        );
    }

    #[test]
    fn cli_default_path_writes_only_to_fixture_project() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("project");
        let cwd = cwd.to_str().unwrap();
        let home = dir.path().join("claude");
        let path = write_claude(&home, CLAUDE_ID, cwd, &claude_lines(CLAUDE_ID, cwd));
        std::fs::create_dir_all(dir.path().join("codex")).unwrap();
        let global = GlobalArgs {
            claude_home: Some(home),
            codex_home: Some(dir.path().join("codex")),
            pi_home: Some(dir.path().join("pi")),
            opencode_home: Some(dir.path().join("opencode")),
        };
        export(
            &global,
            &ExportArgs {
                session: Some(path.to_str().unwrap().to_string()),
                ..ExportArgs::default()
            },
        )
        .unwrap();
        let output = Path::new(cwd).join(format!("exported-sessions/claude/{CLAUDE_ID}.txt"));
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            render_markdown(&Transcript::load(&path, Agent::Claude).unwrap()).unwrap()
        );
    }

    #[test]
    fn pi_and_opencode_export_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("project");
        let cwd = cwd.to_str().unwrap();
        let (pi, oc) = (dir.path().join("pi"), dir.path().join("opencode"));
        write_pi(&pi, PI_ID, cwd, &pi_lines(PI_ID, cwd));
        write_opencode_db(&oc.join("opencode.db"), cwd, 1_000);
        let global = GlobalArgs {
            claude_home: Some(dir.path().join("claude")),
            codex_home: Some(dir.path().join("codex")),
            pi_home: Some(pi),
            opencode_home: Some(oc),
        };
        for (id, agent) in [(PI_ID, "pi"), (OPENCODE_ID, "opencode")] {
            let out = dir.path().join(format!("{agent}.txt"));
            export(
                &global,
                &ExportArgs {
                    session: Some(id.to_string()),
                    output: Some(out.clone()),
                    ..ExportArgs::default()
                },
            )
            .unwrap();
            let text = std::fs::read_to_string(&out).unwrap();
            assert!(text.starts_with("> add a rate limiter"), "{agent}: {text}");
            assert!(text.contains("bash"), "{agent}: {text}");
            assert!(
                !text.contains("Called the Read tool"),
                "synthetic part: {text}"
            );
            if agent == "pi" {
                assert!(text.contains("> use a token bucket"), "{text}");
                assert!(!text.contains("try redis"), "abandoned branch: {text}");
            }
        }
    }

    #[test]
    fn unwritable_default_path_is_named_in_error() {
        let dir = tempfile::tempdir().unwrap();
        // The session's cwd sits under a regular file, so mkdir fails.
        std::fs::write(dir.path().join("file"), "").unwrap();
        let cwd = dir.path().join("file/project");
        let cwd = cwd.to_str().unwrap();
        let home = dir.path().join("claude");
        let path = write_claude(&home, CLAUDE_ID, cwd, &claude_lines(CLAUDE_ID, cwd));
        let global = GlobalArgs {
            claude_home: Some(home),
            codex_home: Some(dir.path().join("codex")),
            pi_home: Some(dir.path().join("pi")),
            opencode_home: Some(dir.path().join("opencode")),
        };
        let err = export(
            &global,
            &ExportArgs {
                session: Some(path.to_str().unwrap().to_string()),
                ..ExportArgs::default()
            },
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").starts_with(&format!("creating {cwd}/exported-sessions/claude: ")),
            "{err:#}"
        );
    }

    #[test]
    fn scalar_tool_results_match_python_representations() {
        let (_dir, transcript) = fixture(
            Agent::Codex,
            &[
                json!({"type":"response_item","payload":{"type":"function_call_output","output":{"a":[true,null,"x"]}}}),
                json!({"type":"response_item","payload":{"type":"function_call_output","output":"{\"output\":false}"}}),
                json!({"type":"response_item","payload":{"type":"function_call_output","output":"{\"output\":42}"}}),
            ],
        );
        assert_eq!(
            render_markdown(&transcript).unwrap(),
            format!("{RESULT}{{'a': [True, None, 'x']}}\n\n{RESULT}(No content)\n\n{RESULT}42\n\n")
        );
    }
}
