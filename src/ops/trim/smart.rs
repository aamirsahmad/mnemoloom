//! `smart-trim` — EXPERIMENTAL. A headless
//! `claude -p` / `codex exec` call picks which long lines of a condensed
//! `LINE N [len=X]` view can be truncated; those lines' content fields are
//! replaced by a summary + prefix placeholder in a new session.
//!
//! The `-i` (handler) path also writes `trim_metadata`, the lineage note
//! and the new session id, and
//! ends with the trim CLI's resume prompt instead of the menu's confirm
//! screen; Codex `exec --json` output is also read from `item.completed`
//! agent messages (current Codex no longer emits `response_item` events).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use super::pystr::py_str;
use super::{
    MIN_TOKEN_SAVINGS, char_len, create_parent, group, inject_lineage, new_session_path, open_menu,
    session_cwd, set_first_line_metadata, take_chars, utc_now_iso,
};
use crate::cli::{AgentArg, GlobalArgs, SmartTrimArgs, StartScreen, agent_of};
use crate::exit::Exit;
use crate::ops::clone::rewrite_session_ids;
use crate::ops::headless::{
    AgentBins, HelperSession, ScratchDir, capture, interrupted_exit, json_lines, last_json_object,
};
use crate::ops::trim::TrimFollowUp;
use crate::session::transcript::{Line, WriteStyle};
use crate::session::{
    self, Agent, Homes, ResolveOptions, Transcript, path_session_uuid, pyjson, resolve,
};

/// Content fields at least this long are truncated.
const SMART_TRIM_THRESHOLD: usize = 500;
/// Condensed entries need at least this much extracted content.
const MIN_ENTRY_LEN: usize = 800;
const TIMEOUT: Duration = Duration::from_secs(600);

const DEFAULT_INSTRUCTIONS: &str =
    "Trim messages that are not relevant to the last task being worked on in this session.";

const PROMPT_HEAD: &str = "I need help identifying which lines can be trimmed from a coding agent session.

The session representation is in: {session_file}

IMPORTANT - FILE FORMAT:
Each entry in that file is on a single line with format: LINE N [len=X]: [TYPE]: <preview>
- N is a LABEL (the original line number in the session file, 0-indexed)
- X is the content length in characters
- TYPE is one of: [ASSISTANT], [USER], or [TOOL_RESULT]
- The entries are NOT consecutive - there are gaps in the N values (e.g., LINE 4, LINE 22, LINE 53...)

CRITICAL: When returning results, use the EXACT value of N from the \"LINE N\" label.
Do NOT use the position of the entry in this file. For example, if you see:
  LINE 4 [len=6394]: ...
  LINE 22 [len=1120]: ...
And you want to trim the second entry, return \"line\": 22 (not \"line\": 2).

I am trying to truncate selected messages to clear out context, but still be able
to continue the work in this session.

================================================================================
TRIMMING INSTRUCTIONS (PAY SPECIAL ATTENTION):
{custom_instructions}
================================================================================

";

const PROMPT_CLAUDE_STRATEGY: &str =
    "Session files can be huge, so you MUST strategically deploy PARALLEL SUB-AGENTS
to analyze different portions of the session. Give each sub-agent the proper
context including the TRIMMING INSTRUCTIONS above so they can accurately identify
which messages can be safely trimmed.
";

const PROMPT_CODEX_STRATEGY: &str =
    "Session files can be huge, so you MUST strategically explore it
to analyze different portions of the session.

Read the session file and identify entries that can be safely trimmed. Focus on:
- Verbose tool results that were one-time analysis only
- Lengthy explanations no longer relevant to current work
- Intermediate debugging output
- Large file reads that served their purpose
";

const PROMPT_TAIL: &str = "
Return your results as a JSON array of objects, where each object has:
- \"line\": the EXACT N value from the \"LINE N\" label (NOT the position in the file)
- \"rationale\": brief reason why it can be trimmed (max 5-6 words)
- \"summary\": 1-2 sentence summary of what the content contains

Example: If the file contains \"LINE 42 [len=5000]: ...\" and you want to trim it:
[
  {\"line\": 42, \"rationale\": \"verbose tool output\", \"summary\": \"Reading config.py\"}
]
";

/// A line the agent chose (0-based index).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub line: usize,
    pub rationale: String,
    pub summary: String,
}

/// Analysis settings.
#[derive(Debug, Clone)]
struct Analysis<'a> {
    /// Which CLI does the analysis.
    cli: Agent,
    instructions: Option<&'a str>,
    content_threshold: usize,
    exclude_types: &'a [String],
    preserve_recent: usize,
}

pub fn run(global: &GlobalArgs, args: &SmartTrimArgs) -> anyhow::Result<ExitCode> {
    let instructions = args.instructions.as_deref().filter(|i| !i.is_empty());
    if let (Some(instr), Some(q)) = (instructions, &args.session) {
        return run_handler(global, q, args.agent, instr);
    }
    let direct = args.exclude_types.as_deref().is_some_and(|e| !e.is_empty())
        || args.preserve_recent != 10
        || args.content_threshold != 200
        || args.output_dir.is_some()
        || args.dry_run;
    if !direct {
        return open_menu(
            global,
            args.session.clone(),
            args.agent,
            StartScreen::SmartTrimForm,
        );
    }
    let Some(q) = &args.session else {
        return Err(Exit::fail(
            "Error: Direct smart-trim options require a session ID\nUse 'mnemoloom smart-trim' without options for interactive mode",
        )
        .into());
    };
    run_cli(global, q, args)
}

/// `smart-trim` with direct flags.
fn run_cli(global: &GlobalArgs, query: &str, args: &SmartTrimArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    let res = resolve(
        query,
        &ResolveOptions::new(&homes).agent(agent_of(args.agent)),
    )?;
    res.agent.require_rewritable("smart-trim")?;
    let exclude: Vec<String> = args
        .exclude_types
        .as_deref()
        .unwrap_or("user")
        .split(',')
        .map(|t| t.trim().to_string())
        .collect();
    let agent = super::detect_agent(&res.session_file)?;
    let name = crate::commands::file_name(&res.session_file);
    println!("🔍 Analyzing session: {name}");
    println!("   Excluding types: {}", exclude.join(", "));
    println!("   Preserving recent: {} messages", args.preserve_recent);
    println!("   Using CLI: {agent}");
    println!();
    let analysis = Analysis {
        cli: agent,
        instructions: None,
        content_threshold: args.content_threshold,
        exclude_types: &exclude,
        preserve_recent: args.preserve_recent,
    };
    let t = Transcript::load(&res.session_file, agent)?;
    let chosen = identify(&t, &analysis, &homes, &AgentBins::from_env())?;
    if chosen.is_empty() {
        println!("✨ No lines identified for trimming");
        return Ok(ExitCode::SUCCESS);
    }
    println!("📊 Identified {} lines for trimming:", chosen.len());
    println!("\n   All {} lines with rationales:", chosen.len());
    for c in &chosen {
        println!("   Line {}: {}", c.line, c.rationale);
        if !c.summary.is_empty() {
            println!("      → {}", c.summary);
        }
    }
    println!();
    if args.dry_run {
        println!("🏃 Dry run mode - no changes made");
        return Ok(ExitCode::SUCCESS);
    }
    let params = json!({
        "method": "smart-trim",
        "exclude_types": exclude,
        "content_threshold": args.content_threshold,
        "preserve_recent": args.preserve_recent,
        "cli_type": agent.as_str(),
    });
    let out = write_smart_trim(
        t,
        &chosen,
        params,
        args.output_dir.as_deref(),
        false,
        &homes,
    )?;
    print_done(&out);
    Ok(ExitCode::SUCCESS)
}

/// `smart-trim -i SESSION` (handler path): defaults
/// (no excluded types, last 10 preserved), 300-token minimum, then resume.
fn run_handler(
    global: &GlobalArgs,
    query: &str,
    agent: Option<AgentArg>,
    instructions: &str,
) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    let res = resolve(query, &ResolveOptions::new(&homes).agent(agent_of(agent)))?;
    res.agent.require_rewritable("smart-trim")?;
    let bins = AgentBins::from_env();
    let cli = if res.agent == Agent::Claude || claude_available(&bins) {
        Agent::Claude
    } else {
        Agent::Codex
    };
    println!("\n🤖 Smart trimming session using {} CLI...", cli.label());
    println!(
        "   Instructions: {}...",
        instructions.chars().take(60).collect::<String>()
    );
    println!("   This may take a minute as the agent analyzes the session...");
    let analysis = Analysis {
        cli,
        instructions: Some(instructions),
        content_threshold: 200,
        exclude_types: &[],
        preserve_recent: 10,
    };
    let t = Transcript::load(&res.session_file, res.agent)?;
    let chosen = identify(&t, &analysis, &homes, &bins)?;
    let mut follow_up = TrimFollowUp {
        agent: res.agent,
        original_id: res.session_id(),
        new: None,
        lines_trimmed: 0,
        tokens_saved: 0,
        cwd: session_cwd(&res.session_file, res.agent),
        original_file: res.session_file.clone(),
    };
    if chosen.is_empty() {
        println!("   Nothing to trim - no changes were made.");
        return crate::menu::after_trim(global, &follow_up, &homes);
    }
    println!("   Found {} lines to trim", chosen.len());
    let params = json!({
        "method": "smart-trim",
        "exclude_types": [],
        "content_threshold": 200,
        "preserve_recent": 10,
        "cli_type": cli.as_str(),
        "instructions": instructions,
    });
    let out = write_smart_trim(t, &chosen, params, None, true, &homes)?;
    follow_up.lines_trimmed = out.stats.num_lines_trimmed;
    follow_up.tokens_saved = out.stats.tokens_saved();
    match out.session_id.clone().zip(out.output_file.clone()) {
        Some(new) => {
            print_done(&out);
            follow_up.new = Some(new);
        }
        None => println!(
            "   Savings too small ({} tokens < {MIN_TOKEN_SAVINGS})",
            out.stats.tokens_saved()
        ),
    }
    crate::menu::after_trim(global, &follow_up, &homes)
}

fn claude_available(bins: &AgentBins) -> bool {
    capture(
        bins,
        Agent::Claude,
        &["--version".to_string()],
        None,
        Some(Duration::from_secs(5)),
    )
    .is_ok_and(|o| o.status.success())
}

/// Smart-trim counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SmartStats {
    pub num_lines_trimmed: u64,
    pub chars_saved: i64,
}

impl SmartStats {
    pub fn tokens_saved(&self) -> i64 {
        self.chars_saved / 4
    }
}

/// A written (or skipped) smart-trim output.
#[derive(Debug, Clone)]
struct Written {
    session_id: Option<String>,
    output_file: Option<PathBuf>,
    stats: SmartStats,
}

/// Apply `chosen` into a new session (new uuid4, `trim_metadata`, lineage
/// note, session ids). With `min_savings`, below 300 tokens nothing is written.
fn write_smart_trim(
    mut t: Transcript,
    chosen: &[Candidate],
    params: Value,
    output_dir: Option<&Path>,
    min_savings: bool,
    homes: &Homes,
) -> anyhow::Result<Written> {
    let input = t.path.clone();
    let agent = t.agent;
    let parent = session::absolute(&input).display().to_string();
    let stats = apply(&mut t, chosen, &parent);
    if min_savings && stats.tokens_saved() < MIN_TOKEN_SAVINGS {
        return Ok(Written {
            session_id: None,
            output_file: None,
            stats,
        });
    }
    let id = uuid::Uuid::new_v4().to_string();
    let output = new_session_path(agent, &input, output_dir, homes, &id);
    let metadata = json!({
        "parent_file": parent,
        "trimmed_at": utc_now_iso(),
        "trim_params": params,
        "stats": {
            "num_lines_trimmed": stats.num_lines_trimmed,
            "tokens_saved": stats.tokens_saved(),
        },
    });
    set_first_line_metadata(
        &mut t.lines,
        Some("continue_metadata"),
        "trim_metadata",
        metadata,
    );
    inject_lineage(agent, &input, &mut t.lines);
    rewrite_session_ids(&mut t, &id);
    create_parent(&output)?;
    t.write_atomic(&output, WriteStyle::Python, None)?;
    Ok(Written {
        session_id: Some(id),
        output_file: Some(output),
        stats,
    })
}

fn print_done(w: &Written) {
    println!("✅ Smart trim complete!");
    println!("   Lines trimmed: {}", w.stats.num_lines_trimmed);
    println!("   Characters saved: {}", group(w.stats.chars_saved));
    println!("   Tokens saved (est): ~{}", group(w.stats.tokens_saved()));
    println!();
    if let (Some(path), Some(id)) = (&w.output_file, &w.session_id) {
        println!("📄 Output: {}", path.display());
        println!("   Session ID: {id}");
    }
}

/// Run the headless analysis and drop protected lines. Failures are
/// reported and yield no candidates; Ctrl-C exits 130.
fn identify(
    t: &Transcript,
    a: &Analysis,
    homes: &Homes,
    bins: &AgentBins,
) -> anyhow::Result<Vec<Candidate>> {
    let protected = protected_lines(&t.lines, a.exclude_types, a.preserve_recent);
    match analyze(t, a, homes, bins) {
        Ok(found) => Ok(found
            .into_iter()
            .filter(|c| !protected.contains(&c.line))
            .collect()),
        Err(Failure::Interrupted) => Err(interrupted_exit().into()),
        Err(Failure::Message(msg)) => {
            println!("   {msg}");
            Ok(Vec::new())
        }
    }
}

/// Why an analysis produced nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Failure {
    /// Reported to the user, analysis yields no lines.
    Message(String),
    /// Ctrl-C while the agent ran.
    Interrupted,
}

impl From<String> for Failure {
    fn from(m: String) -> Failure {
        Failure::Message(m)
    }
}

impl From<&str> for Failure {
    fn from(m: &str) -> Failure {
        Failure::Message(m.to_string())
    }
}

/// Indices never trimmed: lines of an excluded `type` and the last N lines.
fn protected_lines(lines: &[Line], exclude: &[String], preserve_recent: usize) -> Vec<usize> {
    let mut out: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            l.value()
                .and_then(|v| v.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|ty| exclude.iter().any(|e| e == ty))
        })
        .map(|(i, _)| i)
        .collect();
    out.extend(lines.len().saturating_sub(preserve_recent)..lines.len());
    out
}

/// Condensed view, headless call, response parsing. The helper session the
/// agent created is removed on every return path.
fn analyze(
    t: &Transcript,
    a: &Analysis,
    homes: &Homes,
    bins: &AgentBins,
) -> Result<Vec<Candidate>, Failure> {
    let entries = condense(&t.lines, a.content_threshold);
    let id = path_session_uuid(&t.path);
    // The condensed view and debug files go to a private temp dir, removed
    // on return (never `./.claude` / `./.codex` under the process cwd, which
    // is often a repo, or the real `~/.claude`).
    let scratch = ScratchDir::new("mnemoloom-smart-trim-").map_err(cli_error)?;
    let repr_file = scratch
        .write(&format!("{id}.txt"), entries.join("\n").as_bytes())
        .map_err(cli_error)?;
    let instructions = a.instructions.unwrap_or(DEFAULT_INSTRUCTIONS);
    let prompt = build_prompt(a.cli, &repr_file, instructions);
    let args: Vec<String> = match a.cli {
        Agent::Claude | Agent::Pi | Agent::OpenCode => [
            "-p",
            "--no-session-persistence",
            "--output-format",
            "json",
            "--permission-mode",
            "bypassPermissions",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain([prompt.clone()])
        .collect(),
        Agent::Codex => vec!["exec".into(), "--json".into(), prompt.clone()],
    };
    let out =
        capture(bins, a.cli, &args, Some(homes), Some(TIMEOUT)).map_err(|e| match e.kind() {
            ErrorKind::Interrupted => Failure::Interrupted,
            ErrorKind::TimedOut => "CLI timeout after 10 minutes".into(),
            _ => format!("CLI error: {e}").into(),
        })?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _helper = HelperSession {
        agent: a.cli,
        id: helper_id(a.cli, &stdout),
        homes,
    };
    if !out.status.success() {
        return Err(format!(
            "CLI error: {}",
            stderr.chars().take(200).collect::<String>()
        )
        .into());
    }
    let text = response_text(a.cli, &stdout)?;
    let items = extract_json_array(&text)?;
    Ok(candidates(&items))
}

/// A scratch-file IO error, reported like a CLI failure.
fn cli_error(e: std::io::Error) -> Failure {
    format!("CLI error: {e}").into()
}

/// Id of the helper session the agent created (Claude `session_id`,
/// Codex `thread.started.thread_id`).
fn helper_id(cli: Agent, stdout: &str) -> Option<String> {
    match cli {
        Agent::Claude | Agent::Pi | Agent::OpenCode => last_json_object(stdout)?
            .get("session_id")?
            .as_str()
            .map(str::to_string),
        Agent::Codex => json_lines(stdout)
            .find(|v| v.get("type").and_then(Value::as_str) == Some("thread.started"))
            .and_then(|v| v.get("thread_id")?.as_str().map(str::to_string)),
    }
}

/// The analysis prompt (Claude or Codex variant), verbatim.
fn build_prompt(cli: Agent, repr_file: &Path, instructions: &str) -> String {
    let strategy = match cli {
        Agent::Claude | Agent::Pi | Agent::OpenCode => PROMPT_CLAUDE_STRATEGY,
        Agent::Codex => PROMPT_CODEX_STRATEGY,
    };
    let head = PROMPT_HEAD
        .replace("{session_file}", &repr_file.display().to_string())
        .replace("{custom_instructions}", instructions);
    format!("{head}{strategy}{PROMPT_TAIL}")
}

/// `LINE {idx} [len={total}]: {label}: {preview}[ | ...]` per long line.
fn condense(lines: &[Line], threshold: usize) -> Vec<String> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(idx, l)| {
            let relevant = relevant_content(l.value()?, threshold);
            let total: usize = relevant.iter().map(|(_, c)| char_len(c)).sum();
            if relevant.is_empty() || total < MIN_ENTRY_LEN {
                return None;
            }
            let preview = relevant
                .iter()
                .take(2)
                .map(|(label, c)| {
                    format!("{label}: {}", take_chars(c, 300).replace('\n', " ").trim())
                })
                .collect::<Vec<_>>()
                .join(" | ");
            Some(format!("LINE {idx} [len={total}]: {preview}"))
        })
        .collect()
}

/// Labelled strings >= `min` chars.
fn relevant_content(v: &Value, min: usize) -> Vec<(&'static str, String)> {
    let long = |s: Option<&Value>| {
        s.and_then(Value::as_str)
            .filter(|s| char_len(s) >= min)
            .map(str::to_string)
    };
    let texts = |content: Option<&Value>, label: &'static str| -> Vec<(&'static str, String)> {
        content
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|b| b.is_object())
            .filter_map(|b| long(b.get("text")).map(|t| (label, t)))
            .collect()
    };
    let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
    match ty {
        "response_item" => match v.pointer("/payload/type").and_then(Value::as_str) {
            Some("message") => texts(v.pointer("/payload/content"), "[ASSISTANT]"),
            Some("function_call_output") => long(v.pointer("/payload/output"))
                .map(|o| vec![("[TOOL_RESULT]", o)])
                .unwrap_or_default(),
            _ => Vec::new(),
        },
        "message" => texts(v.get("content"), "[ASSISTANT]"),
        "function_call_output" | "tool_result" => {
            let key = if ty == "tool_result" {
                "result"
            } else {
                "output"
            };
            long(v.get(key))
                .map(|o| vec![("[TOOL_RESULT]", o)])
                .unwrap_or_default()
        }
        "assistant" => v
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| long(b.get("text")).map(|t| ("[ASSISTANT]", t)))
            .collect(),
        "user" => v
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|b| b.is_object())
            .filter_map(
                |b| match b.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => long(b.get("text")).map(|t| ("[USER]", t)),
                    "" if b.get("text").is_some() => long(b.get("text")).map(|t| ("[USER]", t)),
                    "tool_result" => long(b.get("content")).map(|t| ("[TOOL_RESULT]", t)),
                    _ => None,
                },
            )
            .collect(),
        _ => Vec::new(),
    }
}

/// Response text: Claude `.result` of the last JSON object line (shell
/// noise skipped); Codex message / agent_message texts concatenated.
fn response_text(cli: Agent, stdout: &str) -> Result<String, String> {
    match cli {
        Agent::Claude | Agent::Pi | Agent::OpenCode => last_json_object(stdout)
            .map(|v| v.get("result").map(py_str).unwrap_or_default())
            .ok_or_else(|| "JSON parse error: no JSON object in CLI output".to_string()),
        Agent::Codex => Ok(json_lines(stdout)
            .map(|v| match v.get("type").and_then(Value::as_str) {
                Some("response_item")
                    if v.pointer("/payload/type").and_then(Value::as_str) == Some("message") =>
                {
                    v.pointer("/payload/content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|b| b.get("text"))
                        .map(|t| t.as_str().unwrap_or(""))
                        .collect::<String>()
                }
                Some("item.completed")
                    if v.pointer("/item/type").and_then(Value::as_str) == Some("agent_message") =>
                {
                    v.pointer("/item/text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                }
                _ => String::new(),
            })
            .collect()),
    }
}

/// First balanced `[...]` in `text`, parsed as JSON.
fn extract_json_array(text: &str) -> Result<Value, String> {
    let start = text.find('[').ok_or("No JSON array found in response")?;
    let mut depth = 0usize;
    let mut end = None;
    for (i, c) in text[start..].char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.ok_or("No matching ] found in response")?;
    serde_json::from_str(&text[start..end]).map_err(|e| format!("JSON parse error: {e}"))
}

/// `(line >= 0, rationale, summary)` items of the parsed array.
fn candidates(items: &Value) -> Vec<Candidate> {
    items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|it| {
            let line = usize::try_from(it.get("line")?.as_i64()?).ok()?;
            let text = |k: &str| it.get(k).map(py_str).unwrap_or_default();
            Some(Candidate {
                line,
                rationale: text("rationale"),
                summary: text("summary"),
            })
        })
        .collect()
}

/// Truncate content fields (>= 500 chars) of the
/// chosen 0-based lines; a line is kept only if it got shorter.
fn apply(t: &mut Transcript, chosen: &[Candidate], parent: &str) -> SmartStats {
    let mut indices: Vec<usize> = chosen.iter().map(|c| c.line).collect();
    indices.sort_unstable();
    indices.dedup();
    let n_lines = t.lines.len();
    let mut stats = SmartStats::default();
    for idx in indices.into_iter().filter(|&i| i < n_lines) {
        let line = &mut t.lines[idx];
        let Some(Value::Object(orig)) = line.value() else {
            continue;
        };
        let summary = chosen
            .iter()
            .find(|c| c.line == idx && !c.summary.is_empty())
            .map(|c| c.summary.as_str());
        let placeholder = Placeholder {
            summary,
            line_num: idx + 1,
            parent,
        };
        let mut data = Value::Object(orig.clone());
        if !trim_fields(&mut data, &placeholder) {
            continue;
        }
        let newline = usize::from(idx + 1 < n_lines || t.trailing_newline);
        let old_len = char_len(&String::from_utf8_lossy(line.raw())) + newline;
        let new_len = char_len(&pyjson::dumps(&data)) + 1;
        let saved = old_len as i64 - new_len as i64;
        if saved > 0 {
            stats.chars_saved += saved;
            stats.num_lines_trimmed += 1;
            line.set_value(data);
        }
    }
    stats
}

struct Placeholder<'a> {
    summary: Option<&'a str>,
    line_num: usize,
    parent: &'a str,
}

impl Placeholder<'_> {
    /// Summary + first 500 chars + pointer, unless that is not shorter.
    fn truncate(&self, content: &str) -> String {
        let len = char_len(content);
        if len <= SMART_TRIM_THRESHOLD {
            return content.to_string();
        }
        let mut parts = Vec::new();
        if let Some(s) = self.summary {
            parts.push(format!("Summary of truncated content: {s}"));
        }
        parts.push(format!(
            "First {SMART_TRIM_THRESHOLD} chars: {}... [truncated]",
            take_chars(content, SMART_TRIM_THRESHOLD)
        ));
        parts.push(format!(
            "See line {} of {} for full content (original was {} chars).",
            self.line_num,
            self.parent,
            group(len as i64)
        ));
        let result = parts.join("\n");
        if char_len(&result) >= len {
            content.to_string()
        } else {
            result
        }
    }

    /// Replace a long string value in place; true when it qualified.
    fn apply_text(&self, v: Option<&mut Value>) -> bool {
        match v {
            Some(Value::String(s)) if char_len(s) >= SMART_TRIM_THRESHOLD => {
                *s = self.truncate(s);
                true
            }
            _ => false,
        }
    }
}

/// Truncate the trimmable fields of one line; true if any qualified.
fn trim_fields(data: &mut Value, p: &Placeholder) -> bool {
    let ty = data
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut trimmed = false;
    match ty.as_str() {
        "assistant" => {
            for item in array_mut(data, "/message/content") {
                if item.get("type").and_then(Value::as_str) == Some("text") {
                    trimmed |= p.apply_text(item.get_mut("text"));
                }
            }
        }
        "user" => {
            for item in array_mut(data, "/message/content") {
                if item.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                let combined = match item.get("content") {
                    Some(Value::Array(parts)) => Some(parts.iter().map(py_str).collect::<String>()),
                    _ => None,
                };
                match combined {
                    Some(c) if char_len(&c) >= SMART_TRIM_THRESHOLD => {
                        item["content"] = Value::String(p.truncate(&c));
                        trimmed = true;
                    }
                    Some(_) => {}
                    None => trimmed |= p.apply_text(item.get_mut("content")),
                }
            }
        }
        "response_item" => match data
            .pointer("/payload/type")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .as_deref()
        {
            Some("message") => {
                for item in array_mut(data, "/payload/content") {
                    trimmed |= p.apply_text(item.get_mut("text"));
                }
            }
            Some("function_call_output") => {
                trimmed |= trim_output(data.pointer_mut("/payload/output"), p);
            }
            _ => {}
        },
        "message" => {
            for item in array_mut(data, "/content") {
                trimmed |= p.apply_text(item.get_mut("text"));
            }
        }
        "function_call_output" => trimmed |= trim_output(data.get_mut("output"), p),
        _ => {}
    }
    trimmed
}

/// `output and len(str(output)) >= 500` -> truncated `str(output)`.
fn trim_output(v: Option<&mut Value>, p: &Placeholder) -> bool {
    let Some(v) = v else { return false };
    let s = py_str(v);
    // Falsy outputs (`""`, `0`, `None`, ...) are always short.
    if char_len(&s) < SMART_TRIM_THRESHOLD {
        return false;
    }
    *v = Value::String(p.truncate(&s));
    true
}

fn array_mut<'a>(data: &'a mut Value, ptr: &str) -> impl Iterator<Item = &'a mut Value> {
    data.pointer_mut(ptr)
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter(|v| v.is_object())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: Value) -> Line {
        Line::from_raw(serde_json::to_vec(&v).unwrap())
    }

    #[test]
    fn condensed_view_format() {
        let long = "x".repeat(900);
        let lines = vec![
            line(
                json!({"type": "assistant", "message": {"content": [{"type": "text", "text": long}]}}),
            ),
            line(
                json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "short"}]}}),
            ),
            line(
                json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": format!("a\nb{}", "y".repeat(850))}]}}),
            ),
            line(
                json!({"type": "response_item", "payload": {"type": "function_call_output", "output": "z".repeat(801)}}),
            ),
        ];
        let out = condense(&lines, 200);
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[0],
            format!("LINE 0 [len=900]: [ASSISTANT]: {}", "x".repeat(300))
        );
        assert!(out[1].starts_with("LINE 2 [len=853]: [TOOL_RESULT]: a by"));
        assert!(out[2].starts_with("LINE 3 [len=801]: [TOOL_RESULT]: zzz"));
    }

    #[test]
    fn prompt_variants() {
        let p = build_prompt(
            Agent::Claude,
            Path::new(".claude/exports/x.txt"),
            "keep auth",
        );
        assert!(p.starts_with("I need help identifying which lines can be trimmed from a coding agent session.\n\nThe session representation is in: .claude/exports/x.txt\n"));
        assert!(p.contains("TRIMMING INSTRUCTIONS (PAY SPECIAL ATTENTION):\nkeep auth\n====="));
        assert!(p.contains("deploy PARALLEL SUB-AGENTS\nto analyze"));
        assert!(p.ends_with("[\n  {\"line\": 42, \"rationale\": \"verbose tool output\", \"summary\": \"Reading config.py\"}\n]\n"));
        let c = build_prompt(Agent::Codex, Path::new("f"), "i");
        assert!(c.contains("- Large file reads that served their purpose\n\nReturn your results"));
        assert!(!c.contains("PARALLEL SUB-AGENTS"));
    }

    #[test]
    fn parses_responses() {
        // Shell-fallback noise around the JSON object is skipped.
        let stdout = "rc noise\n{\"type\":\"result\",\"result\":\"Here: [{\\\"line\\\": 4, \\\"rationale\\\": \\\"r\\\", \\\"summary\\\": \\\"s\\\"}, {\\\"line\\\": -1}] done\",\"session_id\":\"abc\"}\nbye\n";
        let text = response_text(Agent::Claude, stdout).unwrap();
        assert_eq!(helper_id(Agent::Claude, stdout).as_deref(), Some("abc"));
        let items = extract_json_array(&text).unwrap();
        assert_eq!(
            candidates(&items),
            vec![Candidate {
                line: 4,
                rationale: "r".into(),
                summary: "s".into()
            }]
        );
        let codex = "{\"type\":\"thread.started\",\"thread_id\":\"t1\"}\n{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"[{\\\"line\\\": 2}]\"}}\n";
        let text = response_text(Agent::Codex, codex).unwrap();
        assert_eq!(text, "[{\"line\": 2}]");
        assert_eq!(helper_id(Agent::Codex, codex).as_deref(), Some("t1"));
        assert_eq!(
            extract_json_array("no array").unwrap_err(),
            "No JSON array found in response"
        );
        assert_eq!(
            extract_json_array("[[1]").unwrap_err(),
            "No matching ] found in response"
        );
    }

    #[test]
    fn protects_excluded_and_recent() {
        let lines: Vec<Line> = (0..5)
            .map(|i| line(json!({"type": if i == 0 { "user" } else { "assistant" }})))
            .collect();
        let mut p = protected_lines(&lines, &["user".to_string()], 2);
        p.sort_unstable();
        assert_eq!(p, vec![0, 3, 4]);
    }

    #[test]
    fn applies_placeholders_only_when_shorter() {
        let long = "a".repeat(2000);
        let src = [
            json!({"type": "assistant", "message": {"content": [{"type": "text", "text": long}]}}),
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "short"}]}}),
            json!({"type": "response_item", "payload": {"type": "function_call_output", "output": "b".repeat(1500)}}),
        ]
        .iter()
        .map(|v| serde_json::to_string(v).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
        let mut t = Transcript::from_bytes(Path::new("/p.jsonl"), Agent::Claude, src.as_bytes());
        let chosen = [
            Candidate {
                line: 0,
                rationale: "r".into(),
                summary: "big text".into(),
            },
            Candidate {
                line: 1,
                rationale: "r".into(),
                summary: String::new(),
            },
            Candidate {
                line: 2,
                rationale: "r".into(),
                summary: String::new(),
            },
        ];
        let stats = apply(&mut t, &chosen, "/p.jsonl");
        assert_eq!(stats.num_lines_trimmed, 2);
        assert!(!t.lines[1].is_modified());
        let text = t.lines[0].value().unwrap()["message"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            text,
            format!(
                "Summary of truncated content: big text\nFirst 500 chars: {}... [truncated]\nSee line 1 of /p.jsonl for full content (original was 2,000 chars).",
                "a".repeat(500)
            )
        );
        assert!(
            t.lines[2].value().unwrap()["payload"]["output"]
                .as_str()
                .unwrap()
                .starts_with("First 500 chars: bbb")
        );
    }

    #[test]
    fn condensed_view_is_private_and_removed() {
        use crate::session::testutil::*;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
        let src = write_codex(
            &homes.codex,
            CODEX_ID,
            "2026-01-02T12-57-30",
            &codex_lines(CODEX_ID, "/w", "cli"),
        );
        let t = Transcript::load(&src, Agent::Codex).unwrap();
        let seen = dir.path().join("seen");
        // `exec --json <prompt>`: the view path follows "is in: ".
        let fake = dir.path().join("fake-codex");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\np=$(printf '%s\\n' \"$3\" | sed -n 's/^The session representation is in: //p')\n\
                 {{ echo \"$p\"; ls -ld \"$(dirname \"$p\")\" \"$p\" | cut -c1-10; }} > '{}'\n\
                 echo '[]'\n",
                seen.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let bins = AgentBins {
            claude: "/nonexistent".into(),
            codex: fake.into(),
        };
        let a = Analysis {
            cli: Agent::Codex,
            instructions: None,
            content_threshold: 200,
            exclude_types: &[],
            preserve_recent: 10,
        };
        let _ = analyze(&t, &a, &homes, &bins);
        let seen = std::fs::read_to_string(&seen).unwrap();
        let lines: Vec<&str> = seen.lines().collect();
        assert_eq!(lines[1..], ["drwx------", "-rw-------"], "{seen}");
        let view = Path::new(lines[0]);
        assert!(view.starts_with(std::env::temp_dir()));
        assert!(!view.exists() && !view.parent().unwrap().exists());
    }

    #[test]
    fn helper_session_removed_on_failures_with_homes_exported() {
        use crate::session::testutil::*;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
        let src = write_codex(
            &homes.codex,
            CODEX_ID,
            "2026-01-02T12-57-30",
            &codex_lines(CODEX_ID, "/w", "cli"),
        );
        let t = Transcript::load(&src, Agent::Codex).unwrap();
        let helper = homes
            .codex
            .join("sessions/2026/10/29/rollout-2026-10-29T00-00-00-helper-77.jsonl");
        for (exit, want) in [
            (1, "CLI error: boom\n"),
            (0, "No JSON array found in response"),
        ] {
            let fake = dir.path().join(format!("fake-codex-{exit}"));
            std::fs::write(
                &fake,
                format!(
                    "#!/bin/sh\nd=\"${{CODEX_HOME:?}}/sessions/2026/10/29\"; mkdir -p \"$d\"\n\
                     echo '{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"helper-77\"}}}}' > \"$d/rollout-2026-10-29T00-00-00-helper-77.jsonl\"\n\
                     echo '{{\"type\":\"thread.started\",\"thread_id\":\"helper-77\"}}'\n\
                     echo boom >&2\nexit {exit}\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
            let bins = AgentBins {
                claude: "/nonexistent".into(),
                codex: fake.into(),
            };
            let a = Analysis {
                cli: Agent::Codex,
                instructions: None,
                content_threshold: 200,
                exclude_types: &[],
                preserve_recent: 10,
            };
            assert_eq!(
                analyze(&t, &a, &homes, &bins),
                Err(Failure::Message(want.into()))
            );
            assert!(!helper.exists(), "helper rollout left behind (exit {exit})");
        }
    }
}
