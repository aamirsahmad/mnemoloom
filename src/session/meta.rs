//! Fast per-file metadata extraction ([`SessionFile`]).
//!
//! Lines are streamed; JSON is parsed only while something is still
//! unknown, and afterwards only for lines whose raw bytes could carry a
//! needed field (cheap `memmem` prefilters). [`Depth::Header`] stops reading
//! as soon as the identifying fields are known; [`Depth::Full`] reads the
//! whole file (name, line count, last timestamp).

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use memchr::memmem::Finder;
use serde::Deserialize;
use serde_json::Value;

use super::lineage::{Derivation, ParentLink, parent_from_value};
use super::message::{is_meta_text, preview_text};
use super::{Agent, codex_filename_id, session_uuid};

/// How much of a file to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Identity only: ids, cwd, branch, flags, first-line lineage, first
    /// user message when it is near the top. Stops early.
    Header,
    /// Everything, including name (custom title), line count and the last
    /// timestamp. Reads the whole file.
    Full,
}

/// Metadata for one session transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionFile {
    pub path: PathBuf,
    pub agent: Agent,
    /// Id from the file name (Claude stem; Codex rollout suffix).
    pub session_id: String,
    /// Id recorded in content (first `sessionId` / `session_meta.payload.id`
    /// / legacy header `id`).
    pub content_id: Option<String>,
    /// Working directory (first non-empty `cwd`; Codex `session_meta` wins).
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    /// User-assigned name. Claude: last non-empty `custom-title.customTitle`
    /// or `agent-name.agentName` (Full only). Codex: `session_index.jsonl`
    /// thread name (filled by discovery / [`SessionFile::with_codex_name`]).
    pub name: Option<String>,
    /// First content timestamp (Codex: the `session_meta` line's).
    pub created: Option<String>,
    /// Last content timestamp near the end of the file (Full only).
    pub last_timestamp: Option<String>,
    /// File mtime.
    pub modified: SystemTime,
    /// File ctime (Python `st_ctime`).
    pub changed: SystemTime,
    pub size: u64,
    /// Physical line count (Full only).
    pub line_count: Option<usize>,
    /// Recognised session file (or a legacy Codex header).
    pub is_valid: bool,
    /// Claude: has >= 1 `user/assistant/system/tool_use/tool_result` record.
    pub has_conversation: bool,
    /// Sub-agent transcript (Claude `agent-*` / `isSidechain`; Codex
    /// `thread_source=subagent` etc.).
    pub is_sidechain: bool,
    /// First line has `"sessionType": "helper"`.
    pub is_helper: bool,
    /// Codex headless `codex exec` run (`source == "exec"`).
    pub is_exec_run: bool,
    /// Codex legacy 2025 rollout header.
    pub is_legacy_codex: bool,
    /// First-line `trim_metadata` / `continue_metadata`.
    pub parent: Option<ParentLink>,
    /// First real (non-meta) user message, whitespace-collapsed, <= 200 chars.
    pub first_user_message: Option<String>,
    pub depth: Depth,
}

impl SessionFile {
    /// Read metadata for `path`, assumed to be an `agent` transcript (an
    /// opencode virtual path for [`Agent::OpenCode`]).
    pub fn read(path: &Path, agent: Agent, depth: Depth) -> io::Result<SessionFile> {
        match agent {
            Agent::Pi => return super::pi::read_meta(path, depth),
            Agent::OpenCode => return super::opencode::read_meta(path, depth),
            Agent::Claude | Agent::Codex => {}
        }
        let md = fs::metadata(path)?;
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let session_id = match agent {
            Agent::Codex => codex_filename_id(&name).unwrap_or_else(|| session_uuid(&name)),
            _ => session_uuid(&name),
        };
        let mut sf = SessionFile {
            path: path.to_path_buf(),
            agent,
            session_id,
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
            is_sidechain: agent == Agent::Claude && name.starts_with("agent-"),
            is_helper: false,
            is_exec_run: false,
            is_legacy_codex: false,
            parent: None,
            first_user_message: None,
            depth,
        };
        let mut scan = Scanner::new(agent, depth, codex_filename_id(&name));
        let mut reader = BufReader::with_capacity(256 * 1024, File::open(path)?);
        let mut lines = LineCounter::default();
        let mut buf = Vec::new();
        let mut i = 0;
        loop {
            buf.clear();
            if reader.read_until(b'\n', &mut buf)? == 0 {
                break;
            }
            lines.feed(&buf);
            let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
            if !scan.visit(i, line, &mut sf) {
                if depth == Depth::Full {
                    lines.drain(&mut reader)?;
                }
                break;
            }
            i += 1;
        }
        if depth == Depth::Full {
            sf.line_count = Some(lines.count());
            sf.last_timestamp = last_timestamp(&read_tail(path, TAIL_BYTES)?.0);
        }
        scan.finish(&mut sf);
        Ok(sf)
    }

    /// Overlay a Codex thread name (no-op for Claude).
    pub fn with_codex_name(mut self, names: &std::collections::HashMap<String, String>) -> Self {
        if self.agent == Agent::Codex
            && let Some(n) = names.get(&self.session_id.to_lowercase())
        {
            self.name = Some(n.clone());
        }
        self
    }

    /// Resumable via resolution: Claude = valid, not sidechain, not helper,
    /// has cwd. Codex = valid (every valid rollout is eligible).
    /// pi = valid header with a cwd. opencode = not a child (subtask) session.
    pub fn eligible(&self) -> bool {
        match self.agent {
            Agent::Claude => {
                self.is_valid && !self.is_sidechain && !self.is_helper && self.cwd.is_some()
            }
            Agent::Codex => self.is_valid,
            Agent::Pi => self.is_valid && self.cwd.is_some(),
            Agent::OpenCode => self.is_valid && !self.is_sidechain,
        }
    }

    /// How this session was derived (from the first line).
    pub fn derivation(&self) -> Derivation {
        self.parent
            .as_ref()
            .map_or(Derivation::Original, |p| p.kind)
    }

    /// Basename of the cwd (`"unknown"` when absent).
    pub fn project(&self) -> String {
        self.cwd
            .as_deref()
            .and_then(|c| Path::new(c).file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into())
    }

    /// Default export path for this session (see [`super::default_export_path`]).
    pub fn default_export_path(&self) -> PathBuf {
        super::default_export_path(&self.path, self.agent, self.cwd.as_deref())
    }
}

static F_USER: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"user\""));
static F_CUSTOM_TITLE: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"custom-title"));
static F_AGENT_NAME: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"agent-name"));
static F_SIDECHAIN: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"isSidechain\""));
static F_SESSION_ID: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"sessionId"));

/// Lines after which a missing branch / first user message stops mattering.
const GIVE_UP_LINES: usize = 200;
/// Legacy Codex header must appear within this many non-empty lines.
const LEGACY_HEADER_LINES: usize = 25;
const CLAUDE_CONVERSATION_TYPES: &[&str] =
    &["user", "assistant", "system", "tool_use", "tool_result"];

#[derive(Deserialize, Default)]
#[serde(default)]
struct ClaudeProbe {
    #[serde(rename = "type")]
    ty: Option<Value>,
    #[serde(rename = "sessionId")]
    session_id: Option<Value>,
    cwd: Option<Value>,
    #[serde(rename = "gitBranch")]
    git_branch: Option<Value>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<Value>,
    #[serde(rename = "customTitle")]
    custom_title: Option<Value>,
    #[serde(rename = "agentName")]
    agent_name: Option<Value>,
    timestamp: Option<Value>,
    metadata: Option<Value>,
}

struct Scanner {
    agent: Agent,
    depth: Depth,
    filename_id: Option<String>,
    nonblank_seen: usize,
    snapshot_branch: Option<String>,
    seen_session_meta: bool,
}

fn nonblank_str(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

impl Scanner {
    fn new(agent: Agent, depth: Depth, filename_id: Option<String>) -> Self {
        Scanner {
            agent,
            depth,
            filename_id,
            nonblank_seen: 0,
            snapshot_branch: None,
            seen_session_meta: false,
        }
    }

    fn finish(&self, sf: &mut SessionFile) {
        if sf.git_branch.is_none() {
            sf.git_branch.clone_from(&self.snapshot_branch);
        }
    }

    /// Process one line; returns false to stop reading.
    fn visit(&mut self, idx: usize, line: &[u8], sf: &mut SessionFile) -> bool {
        if idx == 0 {
            self.first_line(line, sf);
        }
        if line.trim_ascii().is_empty() {
            return true;
        }
        self.nonblank_seen += 1;
        match self.agent {
            Agent::Claude => self.visit_claude(line, sf),
            Agent::Codex => self.visit_codex(line, sf),
            // Read by `pi::read_meta` / `opencode::read_meta` instead.
            Agent::Pi | Agent::OpenCode => false,
        }
    }

    fn first_line(&self, line: &[u8], sf: &mut SessionFile) {
        let Ok(v) = serde_json::from_slice::<Value>(line.trim_ascii()) else {
            return;
        };
        sf.parent = parent_from_value(&v);
        sf.is_helper = v.get("sessionType").and_then(Value::as_str) == Some("helper");
    }

    fn claude_identity_known(&self, sf: &SessionFile) -> bool {
        sf.content_id.is_some()
            && sf.cwd.is_some()
            && sf.is_valid
            && sf.created.is_some()
            && (sf.git_branch.is_some() || self.nonblank_seen >= GIVE_UP_LINES)
    }

    fn visit_claude(&mut self, line: &[u8], sf: &mut SessionFile) -> bool {
        let identity_known = self.claude_identity_known(sf);
        let want_user = sf.first_user_message.is_none() && self.nonblank_seen <= GIVE_UP_LINES * 5;
        if identity_known
            && self.depth == Depth::Header
            && (!want_user || self.nonblank_seen >= GIVE_UP_LINES)
        {
            return false;
        }
        let maybe_user = want_user && F_USER.find(line).is_some();
        let maybe_name = self.depth == Depth::Full
            && (F_CUSTOM_TITLE.find(line).is_some() || F_AGENT_NAME.find(line).is_some());
        let maybe_sidechain = !sf.is_sidechain
            && F_SIDECHAIN.find(line).is_some()
            && contains_true_after_sidechain(line);
        let maybe_valid = !sf.is_valid && F_SESSION_ID.find(line).is_some();
        if identity_known && !maybe_user && !maybe_name && !maybe_sidechain && !maybe_valid {
            return true;
        }
        let Ok(p) = serde_json::from_slice::<ClaudeProbe>(line) else {
            return true;
        };
        let ty = p.ty.as_ref().and_then(Value::as_str).unwrap_or("");
        let conversation = CLAUDE_CONVERSATION_TYPES.contains(&ty);
        if conversation {
            sf.has_conversation = true;
        }
        if let Some(id) = nonblank_str(p.session_id.as_ref()) {
            if sf.content_id.is_none() {
                sf.content_id = Some(id.trim().to_string());
            }
            if conversation {
                sf.is_valid = true;
            }
        }
        if sf.cwd.is_none()
            && let Some(c) = p
                .cwd
                .as_ref()
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
        {
            sf.cwd = Some(c.to_string());
        }
        if sf.git_branch.is_none()
            && let Some(b) = p
                .git_branch
                .as_ref()
                .and_then(Value::as_str)
                .filter(|b| !b.is_empty())
        {
            sf.git_branch = Some(b.to_string());
        }
        if p.is_sidechain.as_ref().and_then(Value::as_bool) == Some(true) {
            sf.is_sidechain = true;
        }
        if sf.created.is_none()
            && let Some(ts) = p
                .timestamp
                .as_ref()
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
        {
            sf.created = Some(ts.to_string());
        }
        if ty == "file-history-snapshot"
            && let Some(m) = p.metadata.as_ref()
        {
            if self.snapshot_branch.is_none()
                && let Some(b) = m
                    .pointer("/git/branch")
                    .and_then(Value::as_str)
                    .filter(|b| !b.is_empty())
            {
                self.snapshot_branch = Some(b.to_string());
            }
            if m.get("is_sidechain").and_then(Value::as_bool) == Some(true) {
                sf.is_sidechain = true;
            }
        }
        let title = match ty {
            "custom-title" => p.custom_title.as_ref(),
            "agent-name" => p.agent_name.as_ref(),
            _ => None,
        };
        if let Some(t) = title.and_then(Value::as_str).filter(|t| !t.is_empty()) {
            sf.name = Some(t.to_string());
        }
        if ty == "user"
            && sf.first_user_message.is_none()
            && let Ok(v) = serde_json::from_slice::<Value>(line)
        {
            sf.first_user_message = claude_user_text(&v);
        }
        true
    }

    fn visit_codex(&mut self, line: &[u8], sf: &mut SessionFile) -> bool {
        let identity = (self.seen_session_meta || sf.is_legacy_codex) && sf.is_valid;
        if identity && (sf.first_user_message.is_some() || self.nonblank_seen > GIVE_UP_LINES) {
            return false;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            return true;
        };
        if !v.is_object() {
            return true;
        }
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = v.get("payload").filter(|p| p.is_object());
        if sf.created.is_none()
            && let Some(ts) = nonblank_str(v.get("timestamp"))
            && ty != "session_meta"
            && !self.seen_session_meta
        {
            // Provisional; a later session_meta line's timestamp wins.
            sf.created = Some(ts.to_string());
        }
        match (ty, payload) {
            ("session_meta", Some(p)) if !self.seen_session_meta => {
                self.seen_session_meta = true;
                let s = |k: &str| nonblank_str(p.get(k)).map(str::to_string);
                if s("id").is_some() || s("cwd").is_some() || s("timestamp").is_some() {
                    sf.is_valid = true;
                }
                if let Some(id) = s("id") {
                    sf.content_id = Some(id.trim().to_string());
                }
                if let Some(c) = p
                    .get("cwd")
                    .and_then(Value::as_str)
                    .filter(|c| !c.is_empty())
                {
                    sf.cwd = Some(c.to_string());
                }
                if let Some(b) = p
                    .pointer("/git/branch")
                    .and_then(Value::as_str)
                    .filter(|b| !b.is_empty())
                {
                    sf.git_branch = Some(b.to_string());
                }
                if let Some(ts) =
                    nonblank_str(v.get("timestamp")).or_else(|| nonblank_str(p.get("timestamp")))
                {
                    sf.created = Some(ts.to_string());
                }
                let source = p.get("source");
                sf.is_exec_run = source.and_then(Value::as_str) == Some("exec");
                sf.is_sidechain = p.get("thread_source").and_then(Value::as_str)
                    == Some("subagent")
                    || source
                        .and_then(Value::as_object)
                        .is_some_and(|o| o.contains_key("subagent"))
                    || nonblank_str(p.get("parent_thread_id")).is_some();
            }
            ("event_msg" | "response_item", Some(p)) => {
                if nonblank_str(p.get("type")).is_some() {
                    sf.is_valid = true;
                }
                if ty == "response_item" && sf.first_user_message.is_none() {
                    sf.first_user_message = codex_user_text(p);
                }
            }
            ("turn_context", Some(p)) => {
                let has_ctx = ["cwd", "model", "approval_policy"]
                    .iter()
                    .any(|k| nonblank_str(p.get(*k)).is_some())
                    || p.get("sandbox_policy")
                        .is_some_and(|s| s.is_string() || s.is_object());
                if has_ctx {
                    sf.is_valid = true;
                }
                if sf.cwd.is_none()
                    && let Some(c) = p
                        .get("cwd")
                        .and_then(Value::as_str)
                        .filter(|c| !c.is_empty())
                {
                    sf.cwd = Some(c.to_string());
                }
            }
            ("message", _) if sf.first_user_message.is_none() => {
                // Legacy 2025 bare item.
                sf.first_user_message = codex_user_text(&v);
            }
            _ => {}
        }
        if !self.seen_session_meta
            && !sf.is_legacy_codex
            && self.nonblank_seen <= LEGACY_HEADER_LINES
        {
            self.legacy_header(&v, sf);
        }
        true
    }

    fn legacy_header(&self, v: &Value, sf: &mut SessionFile) {
        let (Some(id), Some(ts)) = (nonblank_str(v.get("id")), nonblank_str(v.get("timestamp")))
        else {
            return;
        };
        if self
            .filename_id
            .as_deref()
            .is_some_and(|f| !f.eq_ignore_ascii_case(id))
        {
            return;
        }
        if v.get("git").is_some_and(Value::is_object)
            || v.get("instructions").is_some_and(Value::is_string)
        {
            sf.is_legacy_codex = true;
            sf.is_valid = true;
            sf.content_id.get_or_insert_with(|| id.to_string());
            sf.created.get_or_insert_with(|| ts.to_string());
            if sf.git_branch.is_none() {
                sf.git_branch = v
                    .pointer("/git/branch")
                    .and_then(Value::as_str)
                    .filter(|b| !b.is_empty())
                    .map(str::to_string);
            }
        }
    }
}

/// `"isSidechain"` followed (after optional spaces and `:`) by `true`.
fn contains_true_after_sidechain(line: &[u8]) -> bool {
    F_SIDECHAIN.find_iter(line).any(|pos| {
        let rest = &line[pos + b"\"isSidechain\"".len()..];
        let rest = rest.trim_ascii_start();
        rest.strip_prefix(b":")
            .is_some_and(|r| r.trim_ascii_start().starts_with(b"true"))
    })
}

/// First real user text of a Claude `user` line: string content or first
/// non-empty string/`text` block; `None` for tool results and meta lines.
fn claude_user_text(v: &Value) -> Option<String> {
    if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let text = match v.pointer("/message/content")? {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().find_map(|i| {
            let t = match i {
                Value::String(s) => Some(s.as_str()),
                Value::Object(_) if i.get("type").and_then(Value::as_str) == Some("text") => {
                    i.get("text").and_then(Value::as_str)
                }
                _ => None,
            }?;
            (!t.trim().is_empty()).then(|| t.to_string())
        })?,
        _ => return None,
    };
    if text.trim().is_empty() || is_meta_text(&text) {
        return None;
    }
    Some(preview_text(&text))
}

/// Joined non-meta `input_text` of a Codex user `message` item.
fn codex_user_text(p: &Value) -> Option<String> {
    if p.get("type").and_then(Value::as_str) != Some("message")
        || p.get("role").and_then(Value::as_str) != Some("user")
    {
        return None;
    }
    let parts: Vec<&str> = p
        .get("content")?
        .as_array()?
        .iter()
        .filter(|b| {
            matches!(
                b.get("type").and_then(Value::as_str),
                Some("input_text" | "text")
            )
        })
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .filter(|t| !t.trim().is_empty() && !is_meta_text(t))
        .collect();
    (!parts.is_empty()).then(|| preview_text(&parts.join("\n")))
}

/// Last `timestamp` among the final lines (within the last 64 KiB).
/// Bytes read from the end of a file to find the last timestamp.
const TAIL_BYTES: u64 = 64 * 1024;

/// Physical line counter (Python `sum(1 for _ in f)` semantics).
#[derive(Default)]
struct LineCounter {
    newlines: usize,
    last: Option<u8>,
}

impl LineCounter {
    fn feed(&mut self, chunk: &[u8]) {
        self.newlines += memchr::memchr_iter(b'\n', chunk).count();
        if let Some(b) = chunk.last() {
            self.last = Some(*b);
        }
    }

    /// Count the rest of `reader` without materializing lines.
    fn drain(&mut self, reader: &mut impl BufRead) -> io::Result<()> {
        loop {
            let chunk = reader.fill_buf()?;
            if chunk.is_empty() {
                return Ok(());
            }
            self.feed(chunk);
            let n = chunk.len();
            reader.consume(n);
        }
    }

    fn count(&self) -> usize {
        self.newlines + usize::from(self.last.is_some_and(|b| b != b'\n'))
    }
}

/// The last `max` bytes of a file, and whether earlier bytes were skipped.
pub fn read_tail(path: &Path, max: u64) -> io::Result<(Vec<u8>, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(max)))?;
    let mut out = Vec::new();
    f.take(max).read_to_end(&mut out)?;
    Ok((out, len > max))
}

fn last_timestamp(tail: &[u8]) -> Option<String> {
    tail.rsplit(|&b| b == b'\n')
        .filter(|l| !l.trim_ascii().is_empty())
        .take(64)
        .find_map(|l| {
            #[derive(Deserialize)]
            struct Ts {
                timestamp: Option<Value>,
            }
            serde_json::from_slice::<Ts>(l)
                .ok()?
                .timestamp?
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
}

/// Detect the agent from content (whole file): Codex line types / `record_type: state` / legacy header
/// win; a pi header (`type: session` with `id` and `cwd`) means pi; a
/// non-empty string `sessionId` means Claude.
pub fn detect_agent_from_content(path: &Path) -> Option<Agent> {
    let reader = BufReader::new(File::open(path).ok()?);
    for line in reader.split(b'\n').map_while(Result::ok) {
        let Ok(v) = serde_json::from_slice::<Value>(line.trim_ascii()) else {
            continue;
        };
        let Some(o) = v.as_object() else { continue };
        let ty = o.get("type");
        if ty.is_some_and(|t| !t.is_null() && !t.is_string()) {
            continue;
        }
        let ty = ty.and_then(Value::as_str);
        if matches!(
            ty,
            Some(
                "session_meta"
                    | "response_item"
                    | "event_msg"
                    | "turn_context"
                    | "world_state"
                    | "compacted"
            )
        ) {
            return Some(Agent::Codex);
        }
        if o.get("record_type").and_then(Value::as_str) == Some("state") {
            return Some(Agent::Codex);
        }
        if ty == Some("session") && o.contains_key("id") && o.contains_key("cwd") {
            return Some(Agent::Pi);
        }
        if o.contains_key("id")
            && o.contains_key("timestamp")
            && (o.contains_key("git") || o.contains_key("instructions"))
        {
            return Some(Agent::Codex);
        }
        if o.get("sessionId")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        {
            return Some(Agent::Claude);
        }
    }
    None
}

/// Detect the agent of a user-supplied file: an opencode virtual path,
/// content, then location (under the configured homes, then `/.claude/` /
/// `/.codex/` in the path). A location-only Claude verdict requires a valid
/// session.
pub fn detect_agent(path: &Path, homes: &super::Homes) -> Option<Agent> {
    if super::opencode::split_virtual(path).is_some() {
        return super::opencode::exists(path).then_some(Agent::OpenCode);
    }
    if let Some(a) = detect_agent_from_content(path) {
        return Some(a);
    }
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| super::absolute(path));
    let under = |agent: Agent| {
        homes.has(agent)
            && fs::canonicalize(homes.home(agent)).is_ok_and(|h| resolved.starts_with(h))
    };
    let s = resolved.to_string_lossy();
    let located = if under(Agent::Claude) {
        Some(Agent::Claude)
    } else if under(Agent::Codex) {
        Some(Agent::Codex)
    } else if under(Agent::Pi) {
        Some(Agent::Pi)
    } else if s.contains("/.claude/") {
        Some(Agent::Claude)
    } else if s.contains("/.codex/") {
        Some(Agent::Codex)
    } else {
        None
    };
    match located {
        Some(Agent::Claude) => SessionFile::read(path, Agent::Claude, Depth::Header)
            .ok()
            .filter(|sf| sf.is_valid)
            .map(|_| Agent::Claude),
        other => other,
    }
}

/// User message count: Claude `user` lines whose content is not a
/// list starting with a `tool_result`; Codex `response_item` user messages;
/// pi `user` message entries; opencode `user` message rows.
pub fn count_user_messages(path: &Path, agent: Agent) -> usize {
    match agent {
        Agent::Pi => return super::pi::count_user_messages(path),
        Agent::OpenCode => return super::opencode::count_user_messages(path),
        Agent::Claude | Agent::Codex => {}
    }
    let Ok(f) = File::open(path) else { return 0 };
    let mut reader = BufReader::with_capacity(256 * 1024, f);
    let mut buf = Vec::new();
    let mut n = 0;
    while reader.read_until(b'\n', &mut buf).is_ok_and(|r| r > 0) {
        if F_USER.find(&buf).is_some()
            && let Ok(v) = serde_json::from_slice::<Value>(&buf)
            && is_counted_user(&v, agent)
        {
            n += 1;
        }
        buf.clear();
    }
    n
}

fn is_counted_user(v: &Value, agent: Agent) -> bool {
    match agent {
        Agent::Claude => {
            v.get("type").and_then(Value::as_str) == Some("user")
                && v.pointer("/message/content/0/type").and_then(Value::as_str)
                    != Some("tool_result")
        }
        Agent::Codex => {
            v.get("type").and_then(Value::as_str) == Some("response_item")
                && v.pointer("/payload/type").and_then(Value::as_str) == Some("message")
                && v.pointer("/payload/role").and_then(Value::as_str) == Some("user")
        }
        Agent::Pi | Agent::OpenCode => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    #[test]
    fn claude_full_metadata() {
        let d = tempfile::tempdir().unwrap();
        let p = write_claude(
            d.path(),
            CLAUDE_ID,
            "/w/proj",
            &claude_lines(CLAUDE_ID, "/w/proj"),
        );
        let sf = SessionFile::read(&p, Agent::Claude, Depth::Full).unwrap();
        assert_eq!(sf.session_id, CLAUDE_ID);
        assert_eq!(sf.content_id.as_deref(), Some(CLAUDE_ID));
        assert_eq!(sf.cwd.as_deref(), Some("/w/proj"));
        assert_eq!(sf.git_branch.as_deref(), Some("main"));
        assert_eq!(
            sf.name.as_deref(),
            Some("renamed-later"),
            "last title wins, agent-name counts"
        );
        assert_eq!(
            sf.first_user_message.as_deref(),
            Some("fix the failing spec in user_spec.rb")
        );
        assert_eq!(sf.created.as_deref(), Some("2026-10-27T03:28:25.917Z"));
        assert_eq!(
            sf.last_timestamp.as_deref(),
            Some("2026-10-27T03:30:00.000Z")
        );
        assert_eq!(
            sf.line_count,
            Some(claude_lines(CLAUDE_ID, "/w/proj").len())
        );
        assert!(sf.is_valid && sf.has_conversation && !sf.is_sidechain && !sf.is_helper);
        assert!(sf.eligible());
        assert_eq!(sf.project(), "proj");
        assert_eq!(count_user_messages(&p, Agent::Claude), 2);
        let bytes = fs::read(&p).unwrap();
        assert_eq!(
            read_tail(&p, bytes.len() as u64).unwrap(),
            (bytes.clone(), false)
        );
        assert_eq!(
            read_tail(&p, 2).unwrap(),
            (bytes[bytes.len() - 2..].to_vec(), true)
        );
        assert_eq!(read_tail(&p, 0).unwrap(), (Vec::new(), true));

        let h = SessionFile::read(&p, Agent::Claude, Depth::Header).unwrap();
        assert_eq!(h.cwd, sf.cwd);
        assert_eq!(h.line_count, None);
        assert_eq!(h.name, None);
        assert_eq!(h.first_user_message, sf.first_user_message);
    }

    #[test]
    fn claude_flags() {
        let d = tempfile::tempdir().unwrap();
        let mut lines = claude_lines(CLAUDE_ID, "/w");
        lines[0] = r#"{"type":"summary","sessionType":"helper","trim_metadata":{"parent_file":"/p.jsonl"}}"#.to_string();
        lines.push(format!(r#"{{"type":"user","isSidechain": true,"sessionId":"{CLAUDE_ID}","message":{{"content":"x"}}}}"#));
        let p = write_claude(d.path(), CLAUDE_ID, "/w", &lines);
        let sf = SessionFile::read(&p, Agent::Claude, Depth::Full).unwrap();
        assert!(sf.is_helper && sf.is_sidechain && !sf.eligible());
        assert_eq!(sf.derivation(), Derivation::Trimmed);

        let agent = write_raw(&d.path().join("agent-abc.jsonl"), &claude_lines("x", "/w"));
        assert!(
            SessionFile::read(&agent, Agent::Claude, Depth::Header)
                .unwrap()
                .is_sidechain
        );

        let meta_only = write_raw(
            &d.path().join("m.jsonl"),
            &[r#"{"type":"file-history-snapshot","messageId":"x","snapshot":{}}"#.to_string()],
        );
        let sf = SessionFile::read(&meta_only, Agent::Claude, Depth::Full).unwrap();
        assert!(!sf.is_valid && !sf.has_conversation);
    }

    #[test]
    fn codex_metadata() {
        let d = tempfile::tempdir().unwrap();
        let p = write_codex(
            d.path(),
            CODEX_ID,
            "2026-05-07T12-57-30",
            &codex_lines(CODEX_ID, "/w/web", "exec"),
        );
        let sf = SessionFile::read(&p, Agent::Codex, Depth::Full).unwrap();
        assert_eq!(sf.session_id, CODEX_ID);
        assert_eq!(sf.content_id.as_deref(), Some(CODEX_ID));
        assert_eq!(sf.cwd.as_deref(), Some("/w/web"));
        assert_eq!(sf.git_branch.as_deref(), Some("fix/gql-timeout"));
        assert_eq!(sf.created.as_deref(), Some("2026-05-07T16:58:02.928Z"));
        assert_eq!(sf.first_user_message.as_deref(), Some("hello codex"));
        assert!(sf.is_valid && sf.is_exec_run && !sf.is_sidechain);
        assert_eq!(count_user_messages(&p, Agent::Codex), 2);
        let h = SessionFile::read(&p, Agent::Codex, Depth::Header).unwrap();
        assert_eq!(
            (h.cwd, h.first_user_message),
            (sf.cwd.clone(), sf.first_user_message.clone())
        );

        let sub = codex_lines(CODEX_ID, "/w", r#"{"subagent":{"thread_spawn":{}}}"#);
        let p2 = write_codex(d.path(), CODEX_ID2, "2026-05-07T12-57-31", &sub);
        assert!(
            SessionFile::read(&p2, Agent::Codex, Depth::Header)
                .unwrap()
                .is_sidechain
        );
    }

    #[test]
    fn codex_legacy_header() {
        let d = tempfile::tempdir().unwrap();
        let lines = vec![
            format!(r#"{{"id":"{CODEX_ID}","timestamp":"2025-08-01T10:00:00Z","instructions":"x","git":{{"branch":"dev"}}}}"#),
            r#"{"record_type":"state"}"#.to_string(),
            r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"legacy hi"}]}"#.to_string(),
        ];
        let p = write_codex(d.path(), CODEX_ID, "2025-08-01T10-00-00", &lines);
        let sf = SessionFile::read(&p, Agent::Codex, Depth::Full).unwrap();
        assert!(sf.is_legacy_codex && sf.is_valid);
        assert_eq!(sf.git_branch.as_deref(), Some("dev"));
        assert_eq!(sf.first_user_message.as_deref(), Some("legacy hi"));
        assert_eq!(detect_agent_from_content(&p), Some(Agent::Codex));
    }

    #[test]
    fn detection() {
        let d = tempfile::tempdir().unwrap();
        let c = write_claude(d.path(), CLAUDE_ID, "/w", &claude_lines(CLAUDE_ID, "/w"));
        assert_eq!(detect_agent_from_content(&c), Some(Agent::Claude));
        let x = write_codex(
            d.path(),
            CODEX_ID,
            "2026-05-07T12-57-30",
            &codex_lines(CODEX_ID, "/w", "cli"),
        );
        assert_eq!(detect_agent_from_content(&x), Some(Agent::Codex));
        let pi = write_pi(d.path(), PI_ID, "/w", &pi_lines(PI_ID, "/w"));
        assert_eq!(detect_agent_from_content(&pi), Some(Agent::Pi));
        let junk = write_raw(
            &d.path().join("j.jsonl"),
            &["[1]".into(), "{\"type\":[1],\"sessionId\":\"x\"}".into()],
        );
        assert_eq!(detect_agent_from_content(&junk), None);
        let homes = crate::session::Homes::new(d.path().join("nope"), d.path().join("nope2"));
        assert_eq!(detect_agent(&junk, &homes), None);
    }

    #[test]
    fn sidechain_true_matcher() {
        assert!(contains_true_after_sidechain(br#"{"isSidechain" : true}"#));
        assert!(!contains_true_after_sidechain(
            br#"{"isSidechain":false,"x":true}"#
        ));
    }
}
