//! Transcript state shared by the preview pane and the full-session viewer
//! conversation text wrapped to the viewport, scrolling
//! by display row, original-query match navigation (`n`/`N`) and an in-view
//! `/` search that lands on matches inside long lines.

use std::ops::Range;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

use super::text::wrap_ranges;
use crate::index::extract::tool_use_text;
use crate::index::query::Row;
use crate::index::snippet::{find_ci, keyword_ranges};
use crate::session::lineage::{full_chain, parent_info};
use crate::session::message::{self, Message, MessageKind, Role, display_title};
use crate::session::{Agent, Derivation, Transcript, path_session_uuid};

/// Who a logical line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    User,
    Assistant,
    /// An assistant tool call (`⏺ [Tool: name]` and its inputs).
    Tool,
    Plain,
}

/// One logical line: text without its `> ` / `⏺ ` / indent prefix
/// (a tool call's first line is just the tool name).
#[derive(Debug, Clone, PartialEq)]
pub struct VLine {
    pub kind: Kind,
    /// First line of a message (carries the role label).
    pub first: bool,
    pub text: String,
}

/// One display row: a slice of a logical line.
#[derive(Debug, Clone, PartialEq)]
pub struct VRow {
    pub line: usize,
    pub range: Range<usize>,
    /// First row of its logical line.
    pub first: bool,
}

/// Viewer state (pure; rendering lives in `render`).
#[derive(Debug, Clone, PartialEq)]
pub struct ViewState {
    /// Snapshot of the viewed session (header); independent of refreshes.
    pub row: Row,
    pub lines: Vec<VLine>,
    /// Layout width / height in cells (0 until [`ViewState::layout`]).
    pub width: usize,
    pub height: usize,
    pub rows: Vec<VRow>,
    /// First visible display row.
    pub scroll: usize,
    /// Lowercase keywords of the search query (blue highlights).
    pub keywords: Vec<String>,
    /// Keyword occurrences as (line, byte).
    pub query_hits: Vec<(usize, usize)>,
    pub query_idx: usize,
    /// `/` + Enter on an empty pattern: navigate query matches.
    pub query_nav: bool,
    /// Typing a `/` pattern.
    pub typing: bool,
    pub pattern: String,
    /// Pattern occurrences as (line, byte).
    pub matches: Vec<(usize, usize)>,
    pub match_idx: usize,
}

/// Outcome of a key in the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewEffect {
    Stay,
    Close,
    Quit,
}

/// Width of the right-aligned role gutter, including its trailing space.
pub const GUTTER: usize = 7;

/// Role label drawn in the gutter of a message's first row.
pub fn label(kind: Kind, agent: Agent) -> &'static str {
    match kind {
        Kind::User => "you",
        // The gutter is 6 cells wide.
        Kind::Assistant if agent == Agent::OpenCode => "opc",
        Kind::Assistant => agent.as_str(),
        Kind::Tool => "tool",
        Kind::Plain => "",
    }
}

/// Cells before the text of every row of a `kind` line.
pub fn indent(kind: Kind) -> usize {
    match kind {
        Kind::Plain => 0,
        _ => GUTTER,
    }
}

/// Load a session as conversation text (a `.jsonl` transcript or an
/// opencode session; pi shows its active branch) or raw text.
pub fn load_text(path: &Path, agent: Agent) -> anyhow::Result<Vec<String>> {
    if agent == Agent::OpenCode || path.extension().is_some_and(|e| e == "jsonl") {
        Ok(message_lines(
            Transcript::load(path, agent)?.display_messages(),
        ))
    } else {
        Ok(std::fs::read_to_string(path)?
            .lines()
            .map(str::to_string)
            .collect())
    }
}

/// Preview content with extracted prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewData {
    pub lines: Vec<String>,
    pub prompt: String,
}

const MAX_PREVIEW_LINES_PER_MSG: usize = 8;

/// Load a session as preview text (first msg, ..., last 2-3 msgs) with extracted prompt.
pub fn load_preview_data(path: &Path, agent: Agent) -> anyhow::Result<PreviewData> {
    if agent == Agent::OpenCode || path.extension().is_some_and(|e| e == "jsonl") {
        let msgs = Transcript::load(path, agent)?.display_messages();
        let prompt = msgs
            .iter()
            .find(|m| m.role == Role::User && !m.is_meta && !m.text.trim().is_empty())
            .map(|m| display_title(&m.text))
            .unwrap_or_default();
        let lines = preview_message_lines(msgs);
        Ok(PreviewData { lines, prompt })
    } else {
        let text = std::fs::read_to_string(path)?;
        let raw_lines: Vec<String> = text.lines().map(str::to_string).collect();
        let prompt = raw_lines
            .iter()
            .find(|l| !l.trim().is_empty())
            .map(|l| display_title(l))
            .unwrap_or_default();
        let lines = abbreviate_raw_lines(raw_lines);
        Ok(PreviewData { lines, prompt })
    }
}

/// Load a session as preview text: first message (prompt), ellipsis, and last 2-3 messages.
pub fn load_preview_text(path: &Path, agent: Agent) -> anyhow::Result<Vec<String>> {
    Ok(load_preview_data(path, agent)?.lines)
}

fn abbreviate_raw_lines(lines: Vec<String>) -> Vec<String> {
    if lines.len() <= 20 {
        return lines;
    }
    let mut out = Vec::new();
    out.extend(lines[..8].iter().cloned());
    out.push(String::new());
    out.push("...".to_string());
    out.push(String::new());
    out.extend(lines[lines.len() - 12..].iter().cloned());
    out
}

/// Preview lines: initial user prompt, `...`, and last 2-3 messages.
pub fn preview_message_lines(msgs: impl IntoIterator<Item = Message>) -> Vec<String> {
    let relevant: Vec<Message> = msgs
        .into_iter()
        .filter(|m| {
            (m.role == Role::User || m.role == Role::Assistant)
                && !m.is_meta
                && match m.kind {
                    MessageKind::Text => !m.text.trim().is_empty(),
                    MessageKind::ToolUse => !tool_use_text(m).trim().is_empty(),
                    _ => false,
                }
        })
        .collect();

    if relevant.is_empty() {
        return Vec::new();
    }

    if relevant.len() <= 4 {
        let mut out = Vec::new();
        for m in &relevant {
            let formatted = format_single_message(m, MAX_PREVIEW_LINES_PER_MSG);
            if formatted.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(String::new());
            }
            out.extend(formatted);
        }
        return out;
    }

    let mut out = Vec::new();
    let first = format_single_message(&relevant[0], MAX_PREVIEW_LINES_PER_MSG);
    out.extend(first);

    out.push(String::new());
    out.push("...".to_string());
    out.push(String::new());

    let tail_start = relevant.len().saturating_sub(3);
    for (i, m) in relevant[tail_start..].iter().enumerate() {
        let formatted = format_single_message(m, MAX_PREVIEW_LINES_PER_MSG);
        if formatted.is_empty() {
            continue;
        }
        if i > 0 {
            out.push(String::new());
        }
        out.extend(formatted);
    }

    out
}

fn format_single_message(m: &Message, max_lines: usize) -> Vec<String> {
    let prefix = match m.role {
        Role::User => "> ",
        Role::Assistant => "⏺ ",
        _ => return Vec::new(),
    };
    let text = match m.kind {
        _ if m.is_meta => return Vec::new(),
        MessageKind::Text => m.text.clone(),
        MessageKind::ToolUse => tool_use_text(m),
        _ => return Vec::new(),
    };
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let all_lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let capped = all_lines.len() > max_lines;
    let take = if capped { max_lines } else { all_lines.len() };
    for (j, line) in all_lines.iter().take(take).enumerate() {
        out.push(match j {
            0 => format!("{prefix}{line}"),
            _ if line.is_empty() => String::new(),
            _ => format!("  {line}"),
        });
    }
    if capped {
        out.push("  ...".to_string());
    }
    out
}

/// `> user text` / `⏺ assistant text` blocks; continuation lines indented
/// two spaces; a blank line between messages. Tool calls appear as
/// `[Tool: name]` plus their string inputs; tool output, thinking and
/// injected meta text are omitted.
pub fn conversation_lines<'a>(
    agent: Agent,
    values: impl Iterator<Item = &'a Value>,
) -> Vec<String> {
    message_lines(
        values
            .enumerate()
            .flat_map(|(i, v)| message::parse_line(agent, i, v)),
    )
}

/// [`conversation_lines`] of parsed messages.
fn message_lines(msgs: impl IntoIterator<Item = Message>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in msgs {
        let prefix = match m.role {
            Role::User => "> ",
            Role::Assistant => "⏺ ",
            _ => continue,
        };
        let text = match m.kind {
            _ if m.is_meta => continue,
            MessageKind::Text => m.text.clone(),
            MessageKind::ToolUse => tool_use_text(&m),
            _ => continue,
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(String::new());
        }
        for (j, line) in text.lines().enumerate() {
            out.push(match j {
                0 => format!("{prefix}{line}"),
                _ if line.is_empty() => String::new(),
                _ => format!("  {line}"),
            });
        }
    }
    out
}

/// Classify prefixed text lines (`> `, `⏺ `, continuation, blank).
pub fn parse_lines(text: Vec<String>) -> Vec<VLine> {
    let mut ctx = Kind::Plain;
    text.into_iter()
        .map(|l| {
            let clean = |s: &str| s.replace('\t', "    ").replace('\r', "");
            if let Some(rest) = l.strip_prefix("> ") {
                ctx = Kind::User;
                VLine {
                    kind: ctx,
                    first: true,
                    text: clean(rest),
                }
            } else if let Some(rest) = l.strip_prefix("⏺ ") {
                let tool = rest
                    .strip_prefix("[Tool: ")
                    .and_then(|t| t.strip_suffix(']'));
                ctx = if tool.is_some() {
                    Kind::Tool
                } else {
                    Kind::Assistant
                };
                VLine {
                    kind: ctx,
                    first: true,
                    text: clean(tool.unwrap_or(rest)),
                }
            } else if l.is_empty() {
                VLine {
                    kind: Kind::Plain,
                    first: false,
                    text: String::new(),
                }
            } else if l == "..." || l == "···" {
                ctx = Kind::Plain;
                VLine {
                    kind: Kind::Plain,
                    first: false,
                    text: clean(&l),
                }
            } else {
                let body = if ctx == Kind::Plain {
                    &l[..]
                } else {
                    l.strip_prefix("  ").unwrap_or(&l)
                };
                VLine {
                    kind: ctx,
                    first: false,
                    text: clean(body),
                }
            }
        })
        .collect()
}

/// Render parsed lines as formatted Ratatui `Line`s with gutters, role labels, and agent colors.
pub fn render_preview_lines(lines: &[VLine], agent: Agent) -> Vec<ratatui::text::Line<'static>> {
    let mut out = Vec::new();
    for line in lines {
        let (role_style, base_style) = match line.kind {
            Kind::User => (
                ratatui::style::Style::new().fg(crate::theme::palette().ok),
                crate::theme::text(),
            ),
            Kind::Assistant => (
                ratatui::style::Style::new().fg(crate::index::tui::render::agent_color(agent)),
                crate::theme::text(),
            ),
            Kind::Tool => (crate::theme::dim(), crate::theme::dim()),
            Kind::Plain => (crate::theme::dim(), crate::theme::dim()),
        };
        let mut spans = Vec::new();
        if line.kind != Kind::Plain || line.text == "..." || line.text == "···" {
            let gutter = if line.first {
                format!("{:>w$} ", label(line.kind, agent), w = GUTTER - 1)
            } else {
                " ".repeat(GUTTER)
            };
            spans.push(ratatui::text::Span::styled(
                gutter,
                role_style.add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        spans.push(ratatui::text::Span::styled(line.text.clone(), base_style));
        out.push(ratatui::text::Line::from(spans));
    }
    out
}

fn occurrences(lines: &[VLine], find: impl Fn(&str) -> Vec<Range<usize>>) -> Vec<(usize, usize)> {
    lines
        .iter()
        .enumerate()
        .flat_map(|(i, l)| find(&l.text).into_iter().map(move |r| (i, r.start)))
        .collect()
}

impl ViewState {
    pub fn new(row: Row, text: Vec<String>, keywords: Vec<String>) -> ViewState {
        let lines = parse_lines(text);
        let query_hits = occurrences(&lines, |t| keyword_ranges(t, &keywords));
        ViewState {
            row,
            lines,
            width: 0,
            height: 0,
            rows: Vec::new(),
            scroll: 0,
            keywords,
            query_hits,
            query_idx: 0,
            query_nav: false,
            typing: false,
            pattern: String::new(),
            matches: Vec::new(),
            match_idx: 0,
        }
    }

    /// Wrap to `width` x `height` cells, keeping the top line in view.
    pub fn layout(&mut self, width: usize, height: usize) {
        self.height = height.max(1);
        if width == self.width {
            return;
        }
        let anchor = self.rows.get(self.scroll).map(|r| (r.line, r.range.start));
        self.width = width;
        self.rows = self
            .lines
            .iter()
            .enumerate()
            .flat_map(|(i, l)| {
                let w = width.saturating_sub(indent(l.kind));
                wrap_ranges(&l.text, w)
                    .into_iter()
                    .enumerate()
                    .map(move |(j, range)| VRow {
                        line: i,
                        range,
                        first: j == 0,
                    })
            })
            .collect();
        self.scroll = anchor.map_or(0, |a| self.row_of(a));
    }

    /// Display row containing (line, byte).
    pub fn row_of(&self, pos: (usize, usize)) -> usize {
        self.rows
            .partition_point(|r| (r.line, r.range.start) <= pos)
            .saturating_sub(1)
    }

    fn searching(&self) -> bool {
        !self.pattern.is_empty() || self.query_nav
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.rows.len().saturating_sub(1);
        self.scroll = self.scroll.saturating_add_signed(delta).min(max);
    }

    pub fn page(&self) -> isize {
        self.height as isize
    }

    pub fn top(&mut self) {
        self.scroll = 0;
    }

    pub fn bottom(&mut self) {
        self.scroll = self.rows.len().saturating_sub(self.height);
    }

    /// How far down the viewport is, 0–100 (100 when everything fits).
    pub fn percent(&self) -> usize {
        let max = self.rows.len().saturating_sub(self.height);
        (self.scroll.min(max) * 100).checked_div(max).unwrap_or(100)
    }

    /// Replace the query keywords (the search changed); true when they differ.
    pub fn set_keywords(&mut self, keywords: Vec<String>) -> bool {
        if keywords == self.keywords {
            return false;
        }
        self.query_hits = occurrences(&self.lines, |t| keyword_ranges(t, &keywords));
        self.keywords = keywords;
        self.query_idx = 0;
        true
    }

    /// Scroll to the first query match, if any.
    pub fn seek_first(&mut self) {
        self.query_idx = 0;
        self.jump(self.query_hits.first().copied());
    }

    /// The match shown as current: the `/` pattern's, else the query's.
    pub fn current_hit(&self) -> Option<(usize, usize)> {
        if self.pattern.is_empty() {
            self.query_hits.get(self.query_idx).copied()
        } else {
            self.matches.get(self.match_idx).copied()
        }
    }

    /// `(current, total)` for the match counter (current is 1-based).
    pub fn match_pos(&self) -> (usize, usize) {
        let (list, idx) = if self.pattern.is_empty() {
            (&self.query_hits, self.query_idx)
        } else {
            (&self.matches, self.match_idx)
        };
        (if list.is_empty() { 0 } else { idx + 1 }, list.len())
    }

    fn jump(&mut self, pos: Option<(usize, usize)>) {
        if let Some(p) = pos {
            self.scroll = self.row_of(p);
        }
    }

    /// Next (`forward`) or previous match, wrapping. Uses the `/` pattern
    /// when set, else the original-query hits.
    pub fn step(&mut self, forward: bool) {
        let (list, idx) = if self.pattern.is_empty() {
            (&self.query_hits, &mut self.query_idx)
        } else {
            (&self.matches, &mut self.match_idx)
        };
        if list.is_empty() {
            return;
        }
        let n = list.len();
        *idx = if forward {
            (*idx + 1) % n
        } else {
            (*idx + n - 1) % n
        };
        let p = list[*idx];
        self.jump(Some(p));
    }

    fn clear_search(&mut self) {
        self.pattern.clear();
        self.matches.clear();
        self.match_idx = 0;
        self.query_nav = false;
    }

    /// Lowercased `/` pattern (for highlighting).
    pub fn pattern_lower(&self) -> String {
        self.pattern.to_lowercase()
    }

    /// Logical line number (1-based) at the top of the viewport.
    pub fn top_line(&self) -> usize {
        self.rows.get(self.scroll).map_or(0, |r| r.line) + 1
    }

    fn type_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char(c) => self.pattern.push(c),
            KeyCode::Backspace => {
                self.pattern.pop();
            }
            KeyCode::Esc => self.typing = false,
            KeyCode::Enter => {
                self.typing = false;
                if self.pattern.is_empty() {
                    self.query_nav = true;
                    self.query_idx = 0;
                    self.jump(self.query_hits.first().copied());
                } else {
                    let p = self.pattern_lower();
                    self.matches = occurrences(&self.lines, |t| find_ci(t, &p));
                    self.match_idx = 0;
                    self.jump(self.matches.first().copied());
                }
            }
            _ => {}
        }
    }

    pub fn on_key(&mut self, k: KeyEvent) -> ViewEffect {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return ViewEffect::Quit;
        }
        if self.typing {
            self.type_key(k);
            return ViewEffect::Stay;
        }
        let page = self.page();
        match k.code {
            KeyCode::Char('f' | 'n') => self.step(true),
            KeyCode::Enter if self.searching() => self.step(true),
            KeyCode::Char('d' | 'N') => self.step(false),
            KeyCode::Char('/') => {
                self.clear_search();
                self.typing = true;
            }
            KeyCode::Esc if self.searching() => self.clear_search(),
            KeyCode::Esc | KeyCode::Char(' ' | 'q') => return ViewEffect::Close,
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::PageDown => self.scroll_by(page),
            KeyCode::Home | KeyCode::Char('g') => self.top(),
            KeyCode::End | KeyCode::Char('G') => self.bottom(),
            _ => {}
        }
        ViewEffect::Stay
    }
}

/// The preview pane's transcript of one session.
#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    pub view: ViewState,
    /// Derivation and short parent id of a trimmed / rolled-over session
    /// (or an opencode subtask).
    pub parent: Option<(Derivation, String)>,
    /// Scroll to the first match at the next layout (rows exist only then).
    pub fresh: bool,
}

impl Preview {
    /// Read a session's transcript and its parent link (first line only;
    /// the `parent_id` of an opencode subtask).
    pub fn load(row: Row, keywords: Vec<String>) -> anyhow::Result<Preview> {
        let path = Path::new(&row.file_path);
        let preview_lines = load_preview_text(path, row.agent)?;
        let lines = if keywords.is_empty() {
            preview_lines
        } else {
            let vlines = parse_lines(preview_lines.clone());
            let hits = occurrences(&vlines, |t| keyword_ranges(t, &keywords));
            if !hits.is_empty() {
                preview_lines
            } else {
                load_text(path, row.agent)?
            }
        };
        let parent = if matches!(row.agent, Agent::OpenCode | Agent::Agy) {
            let chain = if row.is_sidechain {
                full_chain(path)
            } else {
                Vec::new()
            };
            match chain.as_slice() {
                [(_, Derivation::Subtask), (p, _), ..] => {
                    Some((Derivation::Subtask, path_session_uuid(p)))
                }
                _ => None,
            }
        } else {
            parent_info(path).map(|p| {
                let id = p
                    .parent_session_id
                    .unwrap_or_else(|| path_session_uuid(&p.parent_file));
                (p.kind, id)
            })
        }
        .map(|(kind, id)| (kind, id.chars().take(8).collect()));
        Ok(Preview {
            view: ViewState::new(row, lines, keywords),
            parent,
            fresh: true,
        })
    }

    pub fn path(&self) -> &str {
        &self.view.row.file_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn row() -> Row {
        Row {
            doc: tantivy::DocAddress::new(0, 0),
            session_id: "s".into(),
            agent: Agent::Claude,
            project: String::new(),
            branch: String::new(),
            cwd: String::new(),
            created: String::new(),
            modified: String::new(),
            modified_ts: 0,
            lines: 0,
            file_path: "/f".into(),
            first_msg_role: String::new(),
            first_msg_content: String::new(),
            last_msg_role: String::new(),
            last_msg_content: String::new(),
            first_user_msg_content: String::new(),
            derivation_type: String::new(),
            is_sidechain: false,
            is_exec_run: false,
            home: String::new(),
            custom_title: String::new(),
        }
    }

    #[test]
    fn claude_conversation() {
        let vals: Vec<Value> = claude_lines(CLAUDE_ID, "/w")
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let lines = conversation_lines(Agent::Claude, vals.iter());
        assert_eq!(
            lines,
            [
                "> fix the failing spec in user_spec.rb",
                "",
                "⏺ [Tool: Bash]",
                "  bundle exec rspec",
                "  Run spec",
                "",
                "⏺ Fixed."
            ]
        );
        let v = ViewState::new(row(), lines, Vec::new());
        assert_eq!(
            v.lines[2],
            VLine {
                kind: Kind::Tool,
                first: true,
                text: "Bash".into()
            }
        );
        assert_eq!(
            v.lines[3],
            VLine {
                kind: Kind::Tool,
                first: false,
                text: "bundle exec rspec".into()
            }
        );
        assert_eq!(v.lines[6].kind, Kind::Assistant);
    }

    #[test]
    fn codex_conversation_has_no_duplicates() {
        let vals: Vec<Value> = codex_lines(CODEX_ID, "/w", "cli")
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let lines = conversation_lines(Agent::Codex, vals.iter());
        assert_eq!(
            lines,
            [
                "> hello codex",
                "",
                "⏺ [Tool: exec_command]",
                "  ls",
                "",
                "⏺ Done."
            ]
        );
    }

    #[test]
    fn navigation_and_search() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let mut v = ViewState::new(row(), lines, vec!["5".into()]);
        v.layout(80, 20);
        assert_eq!(v.query_hits.len(), 19);
        v.on_key(key(KeyCode::Char('f')));
        assert_eq!(v.scroll, 15);
        v.on_key(key(KeyCode::Char('d')));
        assert_eq!(v.scroll, 5);
        v.on_key(key(KeyCode::Char('d')));
        assert_eq!(v.scroll, 95);
        v.on_key(key(KeyCode::End));
        assert_eq!(v.scroll, 80);
        v.on_key(key(KeyCode::PageDown));
        assert_eq!(v.scroll, 99);
        v.on_key(key(KeyCode::Char('/')));
        for c in "LINE 4".chars() {
            v.on_key(key(KeyCode::Char(c)));
        }
        v.on_key(key(KeyCode::Enter));
        assert_eq!((v.matches.len(), v.scroll), (11, 4));
        v.on_key(key(KeyCode::Enter));
        assert_eq!(v.scroll, 40);
        assert_eq!(v.on_key(key(KeyCode::Esc)), ViewEffect::Stay);
        assert!(v.pattern.is_empty());
        v.on_key(key(KeyCode::Char('/')));
        v.on_key(key(KeyCode::Enter));
        assert!(v.query_nav);
        assert_eq!(v.scroll, 5);
        assert_eq!(v.on_key(key(KeyCode::Char('q'))), ViewEffect::Close);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(v.on_key(ctrl_c), ViewEffect::Quit);
    }

    #[test]
    fn long_lines_wrap_and_search_lands_inside() {
        let long = format!("⏺ {} ENDMARK", "x".repeat(30_000));
        let mut v = ViewState::new(row(), vec!["> hi".into(), long], Vec::new());
        v.layout(80, 20);
        let text_w = 80 - indent(Kind::Assistant);
        assert!(v.rows.len() > 30_000 / text_w);
        assert!(v.rows.iter().all(|r| r.range.len() <= text_w));
        // End shows the last rows, which hold ENDMARK.
        v.on_key(key(KeyCode::End));
        let last = v.rows.last().unwrap();
        assert!(v.lines[last.line].text[last.range.clone()].contains("ENDMARK"));
        assert!(v.scroll + v.height >= v.rows.len());
        // Search jumps to the row holding the match, not the line start.
        v.on_key(key(KeyCode::Home));
        v.on_key(key(KeyCode::Char('/')));
        for c in "endmark".chars() {
            v.on_key(key(KeyCode::Char(c)));
        }
        v.on_key(key(KeyCode::Enter));
        let r = &v.rows[v.scroll];
        assert!(v.lines[r.line].text[r.range.clone()].contains("ENDMARK"));
        // Re-layout keeps the position.
        let line = v.rows[v.scroll].line;
        v.layout(40, 20);
        assert_eq!(v.rows[v.scroll].line, line);
        assert!(v.rows.iter().all(|r| r.range.len() <= 40));
    }

    #[test]
    fn pi_preview_shows_the_active_branch() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_pi(dir.path(), PI_ID, "/w", &pi_lines(PI_ID, "/w"));
        let lines = load_text(&p, Agent::Pi).unwrap();
        assert_eq!(
            lines,
            [
                "> add a rate limiter",
                "",
                "⏺ [Tool: bash]",
                "  ls",
                "",
                "⏺ Here is the layout.",
                "",
                "> use a token bucket",
                "",
                "⏺ Token bucket added.",
            ]
        );
        assert_eq!(label(Kind::Assistant, Agent::Pi), "pi");
    }

    #[test]
    fn opencode_preview_and_subtask_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let db = write_opencode_db(&dir.path().join("opencode.db"), "/w", 5000);
        let path = crate::session::opencode::virtual_path(&db, OPENCODE_CHILD_ID);
        let r = Row {
            agent: Agent::OpenCode,
            file_path: path.to_string_lossy().into_owned(),
            is_sidechain: true,
            ..row()
        };
        let p = Preview::load(r, Vec::new()).unwrap();
        assert_eq!(p.view.lines[0].text, "explore the code");
        let short: String = OPENCODE_ID.chars().take(8).collect();
        assert_eq!(p.parent, Some((Derivation::Subtask, short)));
        let parent = crate::session::opencode::virtual_path(&db, OPENCODE_ID);
        let text = load_text(&parent, Agent::OpenCode).unwrap();
        assert_eq!(text[0], "> add a rate limiter");
        assert!(text.contains(&"⏺ Done.".to_string()));
        assert!(!text.iter().any(|l| l.contains("Called the Read tool")));
        // The 6-cell gutter fits the opencode label.
        assert!(label(Kind::Assistant, Agent::OpenCode).len() < GUTTER);
    }

    #[test]
    fn preview_message_lines_short_conversation() {
        let msgs = vec![
            Message::new(0, 0, Role::User, MessageKind::Text, "first prompt".into()),
            Message::new(1, 0, Role::Assistant, MessageKind::Text, "reply 1".into()),
            Message::new(2, 0, Role::User, MessageKind::Text, "second prompt".into()),
        ];
        let lines = preview_message_lines(msgs);
        assert_eq!(
            lines,
            ["> first prompt", "", "⏺ reply 1", "", "> second prompt"]
        );
    }

    #[test]
    fn preview_message_lines_long_conversation_shows_prompt_ellipsis_tail() {
        let msgs: Vec<Message> = (0..8)
            .map(|i| {
                let (role, prefix) = if i % 2 == 0 {
                    (Role::User, "user")
                } else {
                    (Role::Assistant, "agent")
                };
                Message::new(i, 0, role, MessageKind::Text, format!("{prefix} msg {i}"))
            })
            .collect();
        let lines = preview_message_lines(msgs);
        assert_eq!(lines[0], "> user msg 0");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "...");
        assert_eq!(lines[3], "");
        // Tail is the last 3 messages: indices 5 (agent), 6 (user), 7 (agent)
        assert_eq!(lines[4], "⏺ agent msg 5");
        assert_eq!(lines[5], "");
        assert_eq!(lines[6], "> user msg 6");
        assert_eq!(lines[7], "");
        assert_eq!(lines[8], "⏺ agent msg 7");

        let vlines = parse_lines(lines);
        assert_eq!(vlines[0].kind, Kind::User);
        assert_eq!(vlines[2].kind, Kind::Plain);
        assert_eq!(vlines[2].text, "...");
        assert_eq!(vlines[4].kind, Kind::Assistant);

        let rendered = render_preview_lines(&vlines, Agent::Claude);
        assert_eq!(rendered.len(), 9);
    }

    #[test]
    fn preview_message_lines_caps_long_message() {
        let long_text = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let msgs = vec![Message::new(0, 0, Role::User, MessageKind::Text, long_text)];
        let lines = preview_message_lines(msgs);
        assert_eq!(lines.len(), 9);
        assert_eq!(lines[0], "> line 0");
        assert_eq!(lines[7], "  line 7");
        assert_eq!(lines[8], "  ...");
    }
}
