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
use crate::session::message::{self, Message, MessageKind, Role};
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
fn parse_lines(text: Vec<String>) -> Vec<VLine> {
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
        let lines = load_text(path, row.agent)?;
        let parent = if row.agent == Agent::OpenCode {
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
}
