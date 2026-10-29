//! Common message model over Claude, Codex, pi and opencode transcripts.
//!
//! One JSONL line can yield several [`Message`]s (one per content block);
//! each carries `line_index` so callers can map back to the raw line in a
//! [`super::Transcript`] for faithful rewriting.

use std::sync::LazyLock;

use serde_json::Value;

use super::Agent;

/// Speaker of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    /// Claude `type: system` lines.
    System,
    /// Codex `developer` / `system` role messages (injected instructions).
    Developer,
    /// Tool output (Claude `tool_result` blocks, Codex `*_output` items).
    Tool,
}

impl Role {
    /// Lowercase name (`user`, `assistant`, `system`, `developer`, `tool`).
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Developer => "developer",
            Role::Tool => "tool",
        }
    }
}

/// What a message block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// Plain text (`text`, `input_text`, `output_text`, string content).
    Text,
    /// Tool invocation (Claude `tool_use`, Codex `function_call` /
    /// `custom_tool_call` / `web_search_call`).
    ToolUse,
    /// Tool output (Claude `tool_result`, Codex `function_call_output` /
    /// `custom_tool_call_output`).
    ToolResult,
    /// Visible reasoning (Claude `thinking`, Codex `reasoning` summary).
    Thinking,
    /// Claude `redacted_thinking` / encrypted Codex reasoning.
    RedactedThinking,
    /// Image block.
    Image,
    /// Compaction marker (Claude `compact_boundary`, Codex `compacted`).
    Compaction,
    /// Anything else recognized as a message but not modelled.
    Other,
}

/// One message block from a transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// 0-based index of the source line in the transcript.
    pub line_index: usize,
    /// Index of the block within the line's content list (0 for scalars).
    pub block_index: usize,
    pub role: Role,
    pub kind: MessageKind,
    /// Text payload: message text, tool input/arguments (as JSON text),
    /// tool output, or thinking text. Empty when there is none.
    pub text: String,
    /// Tool name for [`MessageKind::ToolUse`]; resolved name for results
    /// when the transcript pairs them (see `Transcript::messages`).
    pub tool_name: Option<String>,
    /// `tool_use.id` / `tool_result.tool_use_id` / Codex `call_id`.
    pub tool_id: Option<String>,
    /// Structured tool input (Claude `input`, parsed Codex `arguments`).
    pub tool_input: Option<Value>,
    /// `tool_result.is_error`.
    pub is_error: bool,
    /// Injected/meta content (`isMeta`, command wrappers, environment
    /// context, AGENTS.md, `[SESSION LINEAGE]`, ...).
    pub is_meta: bool,
    /// Claude `isSidechain: true`.
    pub is_sidechain: bool,
    /// Claude `isCompactSummary: true` (post-compaction summary).
    pub is_compact_summary: bool,
    /// Line `timestamp`, if present.
    pub timestamp: Option<String>,
}

impl Message {
    pub(crate) fn new(
        line_index: usize,
        block_index: usize,
        role: Role,
        kind: MessageKind,
        text: String,
    ) -> Self {
        Message {
            line_index,
            block_index,
            role,
            kind,
            text,
            tool_name: None,
            tool_id: None,
            tool_input: None,
            is_error: false,
            is_meta: false,
            is_sidechain: false,
            is_compact_summary: false,
            timestamp: None,
        }
    }
}

const META_TAGS: &[&str] = &[
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
    "environment_context",
    "user_instructions",
    "user_shell_command",
    "recommended_plugins",
    "skills_instructions",
    "apps_instructions",
    "plugins_instructions",
    "multi_agent_mode",
    "turn_aborted",
];

const META_PREFIXES: &[&str] = &[
    "<environment_context>",
    "<permissions instructions>",
    "<user_instructions>",
    "<turn_aborted>",
    "<user_shell_command>",
    "<recommended_plugins>",
    "<skills_instructions>",
    "<apps_instructions>",
    "<plugins_instructions>",
    "<multi_agent_mode>",
    "<codex_internal_context ",
    "<codex_internal_context>",
    "# AGENTS.md instructions",
];

static META_RES: LazyLock<[regex::Regex; 2]> = LazyLock::new(|| {
    [
        regex::Regex::new(r"(?i)^Caveat:").unwrap(),
        regex::Regex::new(r"(?i)^\s*\[SESSION LINEAGE\]").unwrap(),
    ]
});

/// Injected/meta text (command wrappers, environment context, AGENTS.md,
/// `[SESSION LINEAGE]` blocks, `Caveat:` notes) — never a real prompt.
pub fn is_meta_text(text: &str) -> bool {
    if META_RES.iter().any(|re| re.is_match(text)) {
        return true;
    }
    let t = text.trim();
    if META_TAGS.iter().any(|tag| {
        t.strip_prefix('<')
            .and_then(|r| r.strip_prefix(tag))
            .is_some_and(|r| r.starts_with('>'))
    }) {
        return true;
    }
    META_PREFIXES.iter().any(|p| t.starts_with(p))
}

/// Strip, collapse whitespace runs, and cap at 200 chars (197 + `...`).
pub fn preview_text(text: &str) -> String {
    let collapsed = collapse_ws(text);
    if collapsed.chars().count() > 200 {
        let head: String = collapsed.chars().take(197).collect();
        format!("{head}...")
    } else {
        collapsed
    }
}

/// `<pasted_content id="…">` / `</pasted_content …>` wrapper tags (either may be missing).
static PASTE_TAG_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"</?pasted_content\b[^>]*>").unwrap());

/// Leading `[Image #N]` / `[Pasted text #N +M lines]` placeholder tokens.
static LEADING_TOKENS_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^(?:\s*\[(?:Image|Pasted text) #\d+[^\]]*\])+").unwrap());

/// A `<tag>…</tag>` block for any [`META_TAGS`] tag.
static META_BLOCK_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    let tags = META_TAGS.join("|");
    regex::Regex::new(&format!(r"(?s)<({tags})>(.*?)</({tags})>")).unwrap()
});

/// Meta blocks whose inner text is kept in a title (`/goal` + its args).
const TITLE_KEPT_TAGS: &[&str] = &["command-name", "command-args"];

/// Remove `<pasted_content>` wrapper tags, keeping the pasted text and the
/// original line breaks (for full-message display).
pub fn strip_paste_tags(text: &str) -> String {
    PASTE_TAG_RE.replace_all(text, "").into_owned()
}

/// One-line title from a raw first user message: wrapper tags removed
/// (inner text kept), leading `[Image #N]`-style tokens and [`META_TAGS`]
/// blocks dropped (a slash command keeps `/name args`), whitespace
/// collapsed. Falls back to the collapsed input rather than returning empty.
pub fn display_title(text: &str) -> String {
    let unwrapped = strip_paste_tags(text);
    let no_meta = META_BLOCK_RE.replace_all(&unwrapped, |c: &regex::Captures| {
        if c[1] != c[3] {
            c[0].to_string()
        } else if TITLE_KEPT_TAGS.contains(&&c[1]) {
            format!(" {} ", &c[2])
        } else {
            " ".to_string()
        }
    });
    let title = collapse_ws(&LEADING_TOKENS_RE.replace(&no_meta, ""));
    if title.is_empty() {
        collapse_ws(text)
    } else {
        title
    }
}

fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Flatten tool-result style content (string, or list of `{text}` items).
pub fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.clone()),
                Value::Object(_) => str_field(i, "text").map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Parse one transcript line into messages (empty for non-message lines).
/// opencode "lines" are the synthetic per-message values built by
/// [`super::opencode::transcript_values`].
pub fn parse_line(agent: Agent, line_index: usize, v: &Value) -> Vec<Message> {
    match agent {
        Agent::Claude => parse_claude(line_index, v),
        Agent::Codex => parse_codex(line_index, v),
        Agent::Pi => super::pi::parse_entry(line_index, v),
        Agent::OpenCode => super::opencode::parse_message(line_index, v),
    }
}

fn parse_claude(idx: usize, v: &Value) -> Vec<Message> {
    let ty = str_field(v, "type").unwrap_or("");
    let ts = str_field(v, "timestamp").map(str::to_string);
    let sidechain = v.get("isSidechain").and_then(Value::as_bool) == Some(true);
    let line_meta = v.get("isMeta").and_then(Value::as_bool) == Some(true);
    let compact = v.get("isCompactSummary").and_then(Value::as_bool) == Some(true);
    let mut out = Vec::new();
    match ty {
        "user" | "assistant" => {
            let role = if ty == "user" {
                Role::User
            } else {
                Role::Assistant
            };
            let content = v.get("message").and_then(|m| m.get("content"));
            match content {
                Some(Value::String(s)) => {
                    out.push(Message::new(idx, 0, role, MessageKind::Text, s.clone()));
                }
                Some(Value::Array(blocks)) => {
                    for (bi, b) in blocks.iter().enumerate() {
                        if let Some(m) = claude_block(idx, bi, role, b) {
                            out.push(m);
                        }
                    }
                }
                _ => {}
            }
        }
        "system" => {
            let kind = if str_field(v, "subtype") == Some("compact_boundary") {
                MessageKind::Compaction
            } else {
                MessageKind::Text
            };
            let text = v.get("content").map(content_text).unwrap_or_default();
            let mut m = Message::new(idx, 0, Role::System, kind, text);
            m.is_meta = true;
            out.push(m);
        }
        _ => {}
    }
    for m in &mut out {
        m.timestamp.clone_from(&ts);
        m.is_sidechain = sidechain;
        m.is_compact_summary = compact;
        if line_meta
            || (m.kind == MessageKind::Text && m.role == Role::User && is_meta_text(&m.text))
        {
            m.is_meta = true;
        }
    }
    out
}

fn claude_block(idx: usize, bi: usize, role: Role, b: &Value) -> Option<Message> {
    if let Value::String(s) = b {
        return Some(Message::new(idx, bi, role, MessageKind::Text, s.clone()));
    }
    let m = match str_field(b, "type")? {
        "text" => Message::new(
            idx,
            bi,
            role,
            MessageKind::Text,
            str_field(b, "text").unwrap_or("").to_string(),
        ),
        "tool_use" => {
            let input = b.get("input").cloned();
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
        "tool_result" => {
            let mut m = Message::new(
                idx,
                bi,
                Role::Tool,
                MessageKind::ToolResult,
                b.get("content").map(content_text).unwrap_or_default(),
            );
            m.tool_id = str_field(b, "tool_use_id").map(str::to_string);
            m.is_error = b.get("is_error").and_then(Value::as_bool) == Some(true);
            m
        }
        "thinking" => Message::new(
            idx,
            bi,
            role,
            MessageKind::Thinking,
            str_field(b, "thinking").unwrap_or("").to_string(),
        ),
        "redacted_thinking" => {
            Message::new(idx, bi, role, MessageKind::RedactedThinking, String::new())
        }
        "image" => Message::new(idx, bi, role, MessageKind::Image, String::new()),
        _ => Message::new(idx, bi, role, MessageKind::Other, String::new()),
    };
    Some(m)
}

fn parse_codex(idx: usize, v: &Value) -> Vec<Message> {
    let ts = str_field(v, "timestamp").map(str::to_string);
    let ty = str_field(v, "type").unwrap_or("");
    let mut out = match ty {
        "response_item" => v
            .get("payload")
            .map(|p| codex_item(idx, p))
            .unwrap_or_default(),
        "compacted" => {
            let text = v
                .get("payload")
                .and_then(|p| str_field(p, "message"))
                .unwrap_or("")
                .to_string();
            let mut m = Message::new(idx, 0, Role::System, MessageKind::Compaction, text);
            m.is_meta = true;
            vec![m]
        }
        // Legacy 2025 rollouts: items as bare lines.
        "message"
        | "function_call"
        | "function_call_output"
        | "custom_tool_call"
        | "custom_tool_call_output"
        | "reasoning" => codex_item(idx, v),
        _ => Vec::new(),
    };
    for m in &mut out {
        m.timestamp.clone_from(&ts);
    }
    out
}

fn codex_item(idx: usize, p: &Value) -> Vec<Message> {
    let mut out = Vec::new();
    match str_field(p, "type").unwrap_or("") {
        "message" => {
            let role = match str_field(p, "role").unwrap_or("") {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                _ => Role::Developer,
            };
            if let Some(Value::Array(blocks)) = p.get("content") {
                for (bi, b) in blocks.iter().enumerate() {
                    let kind = match str_field(b, "type") {
                        Some("input_text" | "output_text" | "text") => MessageKind::Text,
                        Some("input_image") => MessageKind::Image,
                        _ => MessageKind::Other,
                    };
                    let text = str_field(b, "text").unwrap_or("").to_string();
                    let mut m = Message::new(idx, bi, role, kind, text);
                    m.is_meta = role == Role::Developer
                        || (kind == MessageKind::Text && is_meta_text(&m.text));
                    out.push(m);
                }
            }
        }
        "function_call" | "custom_tool_call" => {
            let (raw, input) = match (p.get("arguments"), p.get("input")) {
                (Some(Value::String(a)), _) => (a.clone(), serde_json::from_str(a).ok()),
                (_, Some(Value::String(i))) => (i.clone(), Some(Value::String(i.clone()))),
                (Some(other), _) | (_, Some(other)) => (other.to_string(), Some(other.clone())),
                _ => (String::new(), None),
            };
            let mut m = Message::new(idx, 0, Role::Assistant, MessageKind::ToolUse, raw);
            m.tool_name = str_field(p, "name").map(str::to_string);
            m.tool_id = str_field(p, "call_id").map(str::to_string);
            m.tool_input = input;
            out.push(m);
        }
        "web_search_call" => {
            let mut m = Message::new(
                idx,
                0,
                Role::Assistant,
                MessageKind::ToolUse,
                p.get("action").map(Value::to_string).unwrap_or_default(),
            );
            m.tool_name = Some("web_search".into());
            m.tool_input = p.get("action").cloned();
            out.push(m);
        }
        "function_call_output" | "custom_tool_call_output" => {
            let mut m = Message::new(
                idx,
                0,
                Role::Tool,
                MessageKind::ToolResult,
                codex_output_text(p.get("output")),
            );
            m.tool_id = str_field(p, "call_id").map(str::to_string);
            out.push(m);
        }
        "reasoning" => {
            let summary: Vec<&str> = p
                .get("summary")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|s| str_field(s, "text")).collect())
                .unwrap_or_default();
            let kind =
                if summary.is_empty() && p.get("encrypted_content").is_some_and(|e| !e.is_null()) {
                    MessageKind::RedactedThinking
                } else {
                    MessageKind::Thinking
                };
            out.push(Message::new(
                idx,
                0,
                Role::Assistant,
                kind,
                summary.join("\n"),
            ));
        }
        _ => {}
    }
    out
}

/// Codex tool output text: a JSON string with an `output` key yields that
/// value (legacy shape), otherwise the raw string.
pub fn codex_output_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(o)) if o.contains_key("output") => match &o["output"] {
                Value::String(inner) => inner.clone(),
                other => other.to_string(),
            },
            _ => s.clone(),
        },
        Some(Value::Null) | None => String::new(),
        Some(other) => content_text(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn meta_text_rules() {
        assert!(is_meta_text("<command-name>/clear</command-name>"));
        assert!(is_meta_text("  <environment_context>\n<cwd>/x</cwd>"));
        assert!(is_meta_text("# AGENTS.md instructions for /x"));
        assert!(is_meta_text("caveat: the messages below"));
        assert!(is_meta_text("\n\n[SESSION LINEAGE]\n..."));
        assert!(is_meta_text("<codex_internal_context foo=1>"));
        assert!(!is_meta_text("fix the failing spec"));
        assert!(!is_meta_text("<command-namex>"));
    }

    #[test]
    fn preview_truncates() {
        assert_eq!(preview_text("  a \n\n b  "), "a b");
        let long = "x".repeat(250);
        let p = preview_text(&long);
        assert_eq!(p.chars().count(), 200);
        assert!(p.ends_with("..."));
    }

    #[test]
    fn claude_blocks() {
        let line = json!({"type":"assistant","isSidechain":false,"timestamp":"t","message":{"role":"assistant","content":[
            {"type":"thinking","thinking":"hmm","signature":"s"},
            {"type":"text","text":"I'll look."},
            {"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}
        ]}});
        let ms = parse_line(Agent::Claude, 3, &line);
        assert_eq!(ms.len(), 3);
        assert_eq!(ms[0].kind, MessageKind::Thinking);
        assert_eq!(ms[1].text, "I'll look.");
        assert_eq!(ms[2].kind, MessageKind::ToolUse);
        assert_eq!(ms[2].tool_name.as_deref(), Some("Bash"));
        assert_eq!(ms[2].tool_input, Some(json!({"command":"ls"})));
        assert_eq!(ms[2].line_index, 3);
        assert_eq!(ms[2].block_index, 2);
        assert_eq!(ms[0].timestamp.as_deref(), Some("t"));

        let res = json!({"type":"user","message":{"role":"user","content":[
            {"tool_use_id":"toolu_1","type":"tool_result","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}],"is_error":true}
        ]}});
        let ms = parse_line(Agent::Claude, 4, &res);
        assert_eq!(ms[0].role, Role::Tool);
        assert_eq!(ms[0].text, "a\nb");
        assert!(ms[0].is_error);
        assert_eq!(ms[0].tool_id.as_deref(), Some("toolu_1"));

        let meta = json!({"type":"user","isMeta":true,"message":{"role":"user","content":"x"}});
        assert!(parse_line(Agent::Claude, 0, &meta)[0].is_meta);
        let cmd = json!({"type":"user","message":{"role":"user","content":"<command-name>/x</command-name>"}});
        assert!(parse_line(Agent::Claude, 0, &cmd)[0].is_meta);
        let boundary = json!({"type":"system","subtype":"compact_boundary","content":"Conversation compacted"});
        assert_eq!(
            parse_line(Agent::Claude, 0, &boundary)[0].kind,
            MessageKind::Compaction
        );
        assert!(parse_line(Agent::Claude, 0, &json!({"type":"ai-title","aiTitle":"x"})).is_empty());
    }

    #[test]
    fn codex_items() {
        let user = json!({"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[
            {"type":"input_text","text":"<environment_context>x</environment_context>"},
            {"type":"input_text","text":"hello"}]}});
        let ms = parse_line(Agent::Codex, 2, &user);
        assert_eq!(ms.len(), 2);
        assert!(ms[0].is_meta);
        assert!(!ms[1].is_meta);
        assert_eq!(ms[1].role, Role::User);

        let call = json!({"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"ls\"}","call_id":"c1"}});
        let ms = parse_line(Agent::Codex, 3, &call);
        assert_eq!(ms[0].kind, MessageKind::ToolUse);
        assert_eq!(ms[0].tool_input, Some(json!({"cmd":"ls"})));
        assert_eq!(ms[0].tool_id.as_deref(), Some("c1"));

        let out = json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"{\"output\":\"done\",\"metadata\":{}}"}});
        assert_eq!(parse_line(Agent::Codex, 4, &out)[0].text, "done");
        let out2 = json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"plain"}});
        assert_eq!(parse_line(Agent::Codex, 4, &out2)[0].text, "plain");

        let reasoning = json!({"type":"response_item","payload":{"type":"reasoning","summary":[],"content":null,"encrypted_content":"zz"}});
        assert_eq!(
            parse_line(Agent::Codex, 5, &reasoning)[0].kind,
            MessageKind::RedactedThinking
        );
        let dev = json!({"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"x"}]}});
        assert_eq!(parse_line(Agent::Codex, 6, &dev)[0].role, Role::Developer);
        assert!(
            parse_line(
                Agent::Codex,
                7,
                &json!({"type":"event_msg","payload":{"type":"user_message"}})
            )
            .is_empty()
        );
        let legacy = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"hi"}]});
        assert_eq!(parse_line(Agent::Codex, 8, &legacy)[0].text, "hi");
    }

    #[test]
    fn display_title_unwraps_pasted_content() {
        assert_eq!(
            display_title(
                "\n\n<pasted_content id=\"0291\">\nsee https://example.com/docs; build a cli\n</pasted_content>\n"
            ),
            "see https://example.com/docs; build a cli"
        );
        // Close tag carrying the id, text on both sides.
        assert_eq!(
            display_title(
                "add epics;\n\n<pasted_content id=\"ce51\">\n run.sh\nUsage: run.sh [--x]\n</pasted_content id=\"ce51\">\n\n; also see the wiki"
            ),
            "add epics; run.sh Usage: run.sh [--x] ; also see the wiki"
        );
        // Bare tag without id, and a missing close tag.
        assert_eq!(display_title("<pasted_content>hello  world"), "hello world");
        assert_eq!(
            display_title(
                "see\n\n<pasted_content id=\"2318\">\n&#32;[**T-1: alerts**](<https://x>)"
            ),
            "see &#32;[**T-1: alerts**](<https://x>)"
        );
        // Lookalike tags are left alone.
        assert_eq!(display_title("<pasted_contents>x"), "<pasted_contents>x");
    }

    #[test]
    fn display_title_drops_leading_tokens() {
        assert_eq!(
            display_title("[Image #1] why is this red?"),
            "why is this red?"
        );
        assert_eq!(
            display_title("[Image #1] [Image #2]\n[Pasted text #1 +42 lines] fix it"),
            "fix it"
        );
        assert_eq!(
            display_title("<pasted_content id=\"p0\">[Image #3] deploy grafana</pasted_content>"),
            "deploy grafana"
        );
        // Only leading ones: an inline reference stays.
        assert_eq!(
            display_title("日本語 🚀 compare with [Image #1] please"),
            "日本語 🚀 compare with [Image #1] please"
        );
        assert_eq!(display_title("[Image #1]"), "[Image #1]");
    }

    #[test]
    fn display_title_reduces_meta_blocks() {
        let cmd = "<command-message>goal</command-message>\n<command-name>/goal</command-name>\n<command-args>create a new cli tool</command-args>";
        assert_eq!(display_title(cmd), "/goal create a new cli tool");
        let bare = "<command-name>/btw</command-name>\n            <command-message>btw</command-message>\n            <command-args></command-args>";
        assert_eq!(display_title(bare), "/btw");
        assert_eq!(
            display_title("<local-command-caveat>Caveat: ignore</local-command-caveat>\nship it"),
            "ship it"
        );
        assert_eq!(
            display_title("<bash-input>ls</bash-input><bash-stdout>a b</bash-stdout>"),
            "<bash-input>ls</bash-input><bash-stdout>a b</bash-stdout>"
        );
        // Mismatched tag pair is not treated as a block.
        assert_eq!(
            display_title("<command-name>/x</command-args> go"),
            "<command-name>/x</command-args> go"
        );
    }

    #[test]
    fn display_title_plain_and_empty() {
        assert_eq!(
            display_title("  fix the\n\tfailing spec  "),
            "fix the failing spec"
        );
        assert_eq!(display_title(""), "");
        assert_eq!(display_title(" \n "), "");
        assert_eq!(
            display_title("<pasted_content id=\"a\"></pasted_content>"),
            "<pasted_content id=\"a\"></pasted_content>"
        );
    }

    #[test]
    fn strip_paste_tags_keeps_newlines() {
        assert_eq!(
            strip_paste_tags(
                "see\n<pasted_content id=\"1\">\na\n  b\n</pasted_content id=\"1\">\nend"
            ),
            "see\n\na\n  b\n\nend"
        );
    }
}
