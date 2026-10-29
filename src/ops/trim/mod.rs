//! `trim`, `trim-in-place`, `smart-trim`.
//!
//! Trim truncates long tool results / tool inputs (and optionally long
//! assistant messages) into a NEW session with lineage metadata. Lines that
//! are not changed are written back byte-for-byte; changed lines use
//! Python-style JSON formatting ([`crate::session::pyjson`]).

mod claude;
mod codex;
mod inplace;
mod pystr;
mod smart;

use std::collections::{BTreeSet, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use serde_json::{Value, json};

use crate::cli::{
    GlobalArgs, MenuArgs, SmartTrimArgs, StartScreen, TrimArgs, TrimInPlaceArgs, agent_of,
};
use crate::commands::thousands as group;
use crate::exit::Exit;
use crate::session::lineage::{
    Derivation, full_chain, inject_into_first_user_message, injection_note,
};
use crate::session::transcript::{Line, WriteStyle};
use crate::session::{self, Agent, Homes, ResolveOptions, Transcript, resolve};

pub use inplace::{InPlaceResult, trim_session_in_place};

/// Minimum estimated tokens saved for a trim to be kept.
pub const MIN_TOKEN_SAVINGS: i64 = 300;

/// What to trim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrimOptions {
    /// Lowercased tool names whose results are trimmed (`None` = all).
    pub target_tools: Option<BTreeSet<String>>,
    /// Content this long (chars) or longer is trimmed.
    pub threshold: usize,
    /// `+N` trims the first N long assistant messages, `-N` all but the last N.
    pub trim_assistant: Option<i64>,
}

impl Default for TrimOptions {
    fn default() -> Self {
        TrimOptions {
            target_tools: None,
            threshold: 500,
            trim_assistant: None,
        }
    }
}

impl TrimOptions {
    fn tool_selected(&self, name: &str) -> bool {
        self.target_tools
            .as_ref()
            .is_none_or(|t| t.contains(&name.to_lowercase()))
    }
}

/// Trim counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrimStats {
    pub num_tools_trimmed: u64,
    pub num_assistant_trimmed: u64,
    /// May be negative (placeholders longer than the text are counted).
    pub chars_saved: i64,
}

impl TrimStats {
    /// `int(chars_saved / 4)`.
    pub fn tokens_saved(&self) -> i64 {
        self.chars_saved / 4
    }
}

/// Result of [`trim_and_create_session`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrimOutcome {
    pub agent: Agent,
    /// New session id (`None` when nothing worth trimming).
    pub session_id: Option<String>,
    pub output_file: Option<PathBuf>,
    pub stats: TrimStats,
}

impl TrimOutcome {
    pub fn nothing_to_trim(&self) -> bool {
        self.session_id.is_none()
    }
}

/// Trim `input` into a new session
/// file (new uuid4) with `trim_metadata` and the lineage note. Below
/// [`MIN_TOKEN_SAVINGS`] nothing is written.
pub fn trim_and_create_session(
    agent: Option<Agent>,
    input: &Path,
    opts: &TrimOptions,
    output_dir: Option<&Path>,
    homes: &Homes,
) -> anyhow::Result<TrimOutcome> {
    let agent = match agent {
        Some(a) => a,
        None => detect_agent(input)?,
    };
    agent.require_rewritable("trim")?;
    let id = uuid::Uuid::new_v4().to_string();
    let output = new_session_path(agent, input, output_dir, homes, &id);
    let mut t = Transcript::load(input, agent)?;
    let parent = session::absolute(input).display().to_string();
    let stats = trim_lines(agent, &mut t.lines, opts, &parent, Some(&id));
    if stats.tokens_saved() < MIN_TOKEN_SAVINGS {
        return Ok(TrimOutcome {
            agent,
            session_id: None,
            output_file: None,
            stats,
        });
    }
    let metadata = json!({
        "parent_file": parent,
        "trimmed_at": utc_now_iso(),
        "trim_params": {
            "threshold": opts.threshold,
            "tools": opts.target_tools.as_ref().filter(|t| !t.is_empty()),
            "trim_assistant_messages": opts.trim_assistant,
        },
        "stats": {
            "num_tools_trimmed": stats.num_tools_trimmed,
            "num_assistant_trimmed": stats.num_assistant_trimmed,
            "tokens_saved": stats.tokens_saved(),
        },
    });
    set_first_line_metadata(
        &mut t.lines,
        Some("continue_metadata"),
        "trim_metadata",
        metadata,
    );
    inject_lineage(agent, input, &mut t.lines);
    create_parent(&output)?;
    t.write_atomic(&output, WriteStyle::Python, None)?;
    Ok(TrimOutcome {
        agent,
        session_id: Some(id),
        output_file: Some(output),
        stats,
    })
}

/// Trim `lines` in place. `parent_file` is referenced by the
/// truncation notices; `new_session_id` rewrites the session id.
pub fn trim_lines(
    agent: Agent,
    lines: &mut [Line],
    opts: &TrimOptions,
    parent_file: &str,
    new_session_id: Option<&str>,
) -> TrimStats {
    match agent {
        Agent::Claude => claude::process(lines, opts, parent_file, new_session_id),
        Agent::Codex => codex::process(lines, opts, parent_file, new_session_id),
        // Refused by `Agent::require_rewritable` before any trim.
        Agent::Pi | Agent::OpenCode => TrimStats::default(),
    }
}

/// A finished trim, handed to the follow-up UI (`menu::after_trim`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrimFollowUp {
    pub agent: Agent,
    /// The trimmed (source) session file and id.
    pub original_file: PathBuf,
    pub original_id: String,
    /// New session id and file; `None` when nothing was trimmed.
    pub new: Option<(String, PathBuf)>,
    pub lines_trimmed: u64,
    pub tokens_saved: i64,
    /// Session cwd (resume runs there).
    pub cwd: Option<String>,
}

/// `mnemoloom trim [SESSION] [-t] [-l] [-a] [-o] [--agent] [--simple-ui]`.
pub fn trim(global: &GlobalArgs, args: &TrimArgs) -> anyhow::Result<ExitCode> {
    let direct = args.simple_ui
        || args.tools.as_deref().is_some_and(|t| !t.is_empty())
        || args.trim_assistant.is_some_and(|n| n != 0)
        || args.output_dir.is_some();
    if !direct {
        return open_menu(
            global,
            args.session.clone(),
            args.agent,
            StartScreen::TrimMenu,
        );
    }
    let homes = global.homes()?;
    let follow_up = trim_direct(&homes, args)?;
    crate::menu::after_trim(global, &follow_up, &homes)
}

/// The direct `mnemoloom trim` run: resolve, print the
/// parameters and the summary, create the trimmed session.
pub fn trim_direct(homes: &Homes, args: &TrimArgs) -> anyhow::Result<TrimFollowUp> {
    let Some(query) = &args.session else {
        return Err(Exit::fail(
            "Error: Direct trim options require SESSION\nUse 'mnemoloom trim' without options for interactive mode",
        )
        .into());
    };
    let res = resolve(
        query,
        &ResolveOptions::new(homes).agent(agent_of(args.agent)),
    )?;
    res.agent.require_rewritable("trim")?;
    let agent = detect_agent(&res.session_file)?;
    if let Some(given) = agent_of(args.agent).filter(|&a| a != agent) {
        eprintln!("⚠️  Detected {agent} session, ignoring --agent {given} argument");
    }
    let opts = TrimOptions {
        target_tools: args
            .tools
            .as_deref()
            .filter(|t| !t.is_empty())
            .map(parse_tools),
        threshold: args.threshold,
        trim_assistant: args.trim_assistant,
    };
    print_trim_params(agent, &opts, &res.session_file);
    let outcome = trim_and_create_session(
        Some(agent),
        &res.session_file,
        &opts,
        args.output_dir.as_deref(),
        homes,
    )?;
    print_summary(&outcome);
    Ok(TrimFollowUp {
        agent,
        original_id: res.session_id(),
        new: outcome.session_id.clone().zip(outcome.output_file.clone()),
        lines_trimmed: outcome.stats.num_tools_trimmed + outcome.stats.num_assistant_trimmed,
        tokens_saved: outcome.stats.tokens_saved(),
        cwd: session_cwd(&res.session_file, agent),
        original_file: res.session_file,
    })
}

/// `mnemoloom trim-in-place SESSION [-t] [-l] [-a] [-n] [--json]`.
/// With `--json`, errors are printed as `{"error": msg}` (exit 1).
pub fn trim_in_place(global: &GlobalArgs, args: &TrimInPlaceArgs) -> anyhow::Result<ExitCode> {
    inplace::run(global, args)
}

/// `mnemoloom smart-trim [SESSION] [-i] [-e] [-p] [-l] [-o] [-n] [--agent]` (EXPERIMENTAL).
pub fn smart_trim(global: &GlobalArgs, args: &SmartTrimArgs) -> anyhow::Result<ExitCode> {
    smart::run(global, args)
}

/// Open the action menu at `screen` (commands run without direct flags).
fn open_menu(
    global: &GlobalArgs,
    session: Option<String>,
    agent: Option<crate::cli::AgentArg>,
    screen: StartScreen,
) -> anyhow::Result<ExitCode> {
    crate::menu::menu(
        global,
        &MenuArgs {
            session,
            agent,
            start_screen: screen,
            ..Default::default()
        },
    )
}

fn print_trim_params(agent: Agent, opts: &TrimOptions, input: &Path) {
    match &opts.target_tools {
        Some(t) => eprintln!(
            "Trimming tools: {}",
            t.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        None => eprintln!("Trimming all tools (no --tools specified)"),
    }
    eprintln!("Agent: {agent}");
    eprintln!("Length threshold: {} characters", opts.threshold);
    match opts.trim_assistant {
        Some(n) if n > 0 => {
            eprintln!("Trimming first {n} assistant messages exceeding threshold")
        }
        Some(n) if n < 0 => eprintln!(
            "Trimming all assistant messages except last {} exceeding threshold",
            n.unsigned_abs()
        ),
        _ => {}
    }
    eprintln!("\nInput: {}", input.display());
}

fn print_summary(o: &TrimOutcome) {
    let rule = "=".repeat(70);
    let none = || "None".to_string();
    println!("\n{rule}\nTRIM SUMMARY\n{rule}");
    println!("Agent: {}", o.agent);
    println!("Tool results trimmed: {}", o.stats.num_tools_trimmed);
    println!(
        "Assistant messages trimmed: {}",
        o.stats.num_assistant_trimmed
    );
    println!("Characters saved: {}", group(o.stats.chars_saved));
    println!("Estimated tokens saved: {}", group(o.stats.tokens_saved()));
    println!();
    println!(
        "Output file: {}",
        o.output_file
            .as_ref()
            .map_or_else(none, |p| p.display().to_string())
    );
    println!();
    println!("Session UUID:");
    println!("{}", o.session_id.clone().unwrap_or_else(none));
    println!("{rule}");
}

/// Split a non-empty `--tools` (lowercased, trimmed; empty names kept).
/// Callers map an empty `--tools` to "all tools".
fn parse_tools(tools: &str) -> BTreeSet<String> {
    tools.split(',').map(|t| t.trim().to_lowercase()).collect()
}

/// Session cwd: Claude = first line carrying `cwd`;
/// Codex = `session_meta.payload.cwd`.
pub(crate) fn session_cwd(path: &Path, agent: Agent) -> Option<String> {
    let t = Transcript::load(path, agent).ok()?;
    t.lines
        .iter()
        .filter_map(Line::value)
        .find_map(|v| match agent {
            Agent::Claude => v.get("cwd").map(|c| c.as_str().unwrap_or("").to_string()),
            Agent::Codex => (v.get("type").and_then(Value::as_str) == Some("session_meta"))
                .then(|| v.pointer("/payload/cwd"))
                .flatten()
                .map(|c| c.as_str().unwrap_or("").to_string()),
            Agent::Pi | Agent::OpenCode => None,
        })
}

/// Positive format marker of one parsed line.
fn agent_indicator(v: &Value) -> Option<Agent> {
    let ty = v.get("type").and_then(Value::as_str);
    if ty == Some("session_meta") || (ty == Some("response_item") && v.get("payload").is_some()) {
        return Some(Agent::Codex);
    }
    if v.get("sessionId").is_some()
        || (matches!(ty, Some("user" | "assistant")) && v.get("message").is_some())
    {
        return Some(Agent::Claude);
    }
    None
}

/// First positive indicator in the first `limit` lines (all when `None`).
fn scan_agent(path: &Path, limit: Option<usize>) -> anyhow::Result<Option<Agent>> {
    let mut reader =
        BufReader::new(File::open(path).with_context(|| format!("reading {}", path.display()))?);
    let mut buf = Vec::new();
    let mut n = 0;
    while limit.is_none_or(|l| n < l) {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        n += 1;
        if let Ok(v) = serde_json::from_slice::<Value>(&buf)
            && let Some(a) = agent_indicator(&v)
        {
            return Ok(Some(a));
        }
    }
    Ok(None)
}

/// Agent from content: first 20 lines, default Claude.
pub fn detect_agent(path: &Path) -> anyhow::Result<Agent> {
    Ok(scan_agent(path, Some(20))?.unwrap_or(Agent::Claude))
}

/// Agent from content: whole file, no default.
fn detect_agent_strict(path: &Path) -> anyhow::Result<Option<Agent>> {
    scan_agent(path, None)
}

/// Output path for a new trimmed session. Claude:
/// `(output_dir|input dir)/<id>.jsonl`. Codex:
/// `(output_dir|<codex home>/sessions)/YYYY/MM/DD/rollout-<local ts>-<id>.jsonl`
/// (the root is not derived from the input's ancestors, which would escape
/// the Codex home for `archived_sessions/` inputs).
fn new_session_path(
    agent: Agent,
    input: &Path,
    output_dir: Option<&Path>,
    homes: &Homes,
    id: &str,
) -> PathBuf {
    let suffix = input
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    match agent {
        Agent::Claude => output_dir
            .unwrap_or(input.parent().unwrap_or(Path::new("")))
            .join(format!("{id}{suffix}")),
        Agent::Codex => {
            let now = chrono::Local::now();
            let root = output_dir.map_or_else(|| homes.codex_sessions(), Path::to_path_buf);
            root.join(now.format("%Y/%m/%d").to_string()).join(format!(
                "rollout-{}-{id}{suffix}",
                now.format("%Y-%m-%dT%H-%M-%S")
            ))
        }
        Agent::Pi | Agent::OpenCode => unreachable!("refused by require_rewritable"),
    }
}

/// `mkdir -p` the parent of `path`.
fn create_parent(path: &Path) -> anyhow::Result<()> {
    match path.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))
        }
        None => Ok(()),
    }
}

/// Set `key` on the first line (dropping `drop`, keeping key order like
/// Python `dict.update`). Returns false when the first line is not a JSON object.
pub(crate) fn set_first_line_metadata(
    lines: &mut [Line],
    drop: Option<&str>,
    key: &str,
    value: Value,
) -> bool {
    let Some(first) = lines.first_mut() else {
        return false;
    };
    let Some(Value::Object(m)) = first.value() else {
        return false;
    };
    let mut m = m.clone();
    if let Some(d) = drop {
        m.shift_remove(d);
    }
    m.insert(key.to_string(), value);
    first.set_value(Value::Object(m));
    true
}

/// Prepend the `[SESSION LINEAGE]` note (input's chain) to the
/// first user message.
fn inject_lineage(agent: Agent, input: &Path, lines: &mut [Line]) {
    if let Some(note) = injection_note(&full_chain(input), Derivation::Trimmed) {
        inject_into_first_user_message(agent, lines, &note);
    }
}

/// Python `datetime.now(timezone.utc).isoformat()`.
pub(crate) fn utc_now_iso() -> String {
    let now = chrono::Utc::now();
    if now.timestamp_subsec_micros() == 0 {
        now.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    } else {
        now.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
    }
}

/// Selected 1-based line numbers for `trim_assistant` (first pass).
fn select_assistant(candidates: &[usize], n: i64) -> HashSet<usize> {
    let k = candidates.len().min(n.unsigned_abs() as usize);
    let count = match n.signum() {
        1 => k,
        -1 => candidates.len() - k,
        _ => 0,
    };
    candidates[..count].iter().copied().collect()
}

/// Content carrying a placeholder from an earlier trim.
fn already_trimmed(s: &str) -> bool {
    s.contains("[...truncated") || s.contains("[Assistant message trimmed")
}

// Trim thresholds and savings count Python characters, not terminal cells.
fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn take_chars(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

/// Notice appended to a truncated tool result (Claude and Codex).
fn tool_result_notice(len: usize, threshold: usize, line_num: usize, parent: &str) -> String {
    format!(
        "\n\n[...truncated - original content was {} characters, showing first {threshold}. See line {line_num} of {parent} for full content]",
        group(len as i64)
    )
}

#[cfg(test)]
mod tests;
