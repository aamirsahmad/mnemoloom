//! Picker state and key handling, independent of the terminal.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use ratatui::widgets::ListState;

use super::ansi;
use super::matcher::{Filter, Haystack};
use super::search::{Hit, Hits, Request};
use crate::mux::model::Agent;

/// Quiet time after the last keystroke before the conversation search runs.
pub const DEBOUNCE: Duration = Duration::from_millis(150);
/// Rank bonus for matching on metadata: such agents list before
/// conversation-only matches.
const META_BONUS: f64 = 1e6;

/// Which part of the picker takes typed keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Typing edits the filter query.
    Filter,
    /// Letters are commands (j/k, r, p, ?, q…).
    Agents,
}

/// What the event loop should do after a key.
#[derive(Debug, PartialEq)]
pub enum Action {
    None,
    Quit,
    Refresh,
    Jump(String),
}

pub struct App {
    pub agents: Vec<Agent>,
    /// Searchable metadata per agent.
    haystacks: Vec<Haystack>,
    pub query: String,
    filter: Filter,
    /// Indices into `agents` that match `query`: best match first, then
    /// urgency order.
    pub visible: Vec<usize>,
    /// Conversation hits for `query`, by lowercase session id.
    hits: Hits,
    /// Bumped whenever `hits` need a new search; replies for an older
    /// value are stale.
    seq: u64,
    /// When the search for `seq` is due (`None`: sent, or nothing to send).
    due: Option<Instant>,
    /// The conversation index is open.
    pub index_ready: bool,
    /// The background index update is running.
    pub indexing: bool,
    /// The selection follows the best match (no move since the last edit).
    follow: bool,
    pub list: ListState,
    pub preview_pane: String,
    pub preview: Vec<Line<'static>>,
    pub scanning: bool,
    /// A background failure to show in the status line.
    pub error: Option<String>,
    pub mode: Mode,
    /// The key-help overlay is open.
    pub help: bool,
    /// The terminal is wide enough to show the preview by default (set on
    /// each draw).
    pub wide: bool,
    /// The user's `p` choice, kept across resizes; `None` follows `wide`.
    pub preview_choice: Option<bool>,
}

const PAGE: usize = 10;

impl App {
    pub fn new(agents: Vec<Agent>) -> Self {
        let mut app = App {
            agents: Vec::new(),
            haystacks: Vec::new(),
            query: String::new(),
            filter: Filter::default(),
            visible: Vec::new(),
            hits: Hits::new(),
            seq: 0,
            due: None,
            index_ready: false,
            indexing: false,
            follow: true,
            list: ListState::default(),
            preview_pane: String::new(),
            preview: Vec::new(),
            scanning: false,
            error: None,
            // The picker opens with an empty query, so typing filters at once.
            mode: Mode::Filter,
            help: false,
            wide: true,
            preview_choice: None,
        };
        app.set_agents(agents);
        app
    }

    /// Replace the rows (after a rescan), keeping the query and, when it is
    /// still listed, the highlighted pane.
    pub fn set_agents(&mut self, agents: Vec<Agent>) {
        let keep = self.selected_pane().map(str::to_owned);
        self.haystacks = agents.iter().map(Haystack::of).collect();
        self.agents = agents;
        self.refilter(keep.as_deref());
        // New sessions may match; the current hits stay until then.
        self.schedule(Instant::now());
    }

    /// The index opened (or is gone): search it now, or drop its hits.
    pub fn set_index_ready(&mut self, ready: bool) {
        self.index_ready = ready;
        if ready {
            self.schedule(Instant::now());
        } else if !self.hits.is_empty() {
            self.hits.clear();
            self.refilter_follow();
        }
    }

    /// The conversation search to start now, if one is due.
    pub fn search_due(&mut self, now: Instant) -> Option<Request> {
        if self.due.is_none_or(|t| t > now) {
            return None;
        }
        self.due = None;
        let conversation = self.filter.conversation().filter(|_| self.index_ready)?;
        let sessions: Vec<String> = self
            .agents
            .iter()
            .filter(|a| !a.session_id.is_empty())
            .map(|a| a.session_id.clone())
            .collect();
        (!sessions.is_empty()).then_some(Request {
            seq: self.seq,
            conversation,
            sessions,
        })
    }

    /// Apply a search reply; a stale one (the query, agents or index
    /// changed since it was asked) is dropped. Returns whether it applied.
    pub fn set_hits(&mut self, seq: u64, hits: Hits) -> bool {
        if seq != self.seq {
            return false;
        }
        self.hits = hits;
        self.refilter_follow();
        true
    }

    /// The selected agent's conversation hit, if any.
    pub fn hit(&self, agent: &Agent) -> Option<&Hit> {
        if agent.session_id.is_empty() {
            return None;
        }
        self.hits.get(&agent.session_id.to_lowercase())
    }

    /// Ask for a new search at `at`; replies to earlier ones become stale.
    fn schedule(&mut self, at: Instant) {
        self.seq += 1;
        self.due = Some(at);
    }

    /// Refilter; keep the highlighted pane unless it follows the best match.
    fn refilter_follow(&mut self) {
        let keep = if self.follow {
            None
        } else {
            self.selected_pane().map(str::to_owned)
        };
        self.refilter(keep.as_deref());
    }

    pub fn selected(&self) -> Option<&Agent> {
        let i = *self.visible.get(self.list.selected()?)?;
        self.agents.get(i)
    }

    pub fn selected_pane(&self) -> Option<&str> {
        self.selected().map(|a| a.pane.as_str())
    }

    pub fn show_preview(&self) -> bool {
        self.preview_choice.unwrap_or(self.wide)
    }

    pub fn set_preview(&mut self, pane: String, text: &str) {
        self.preview = ansi::to_lines(text);
        self.preview_pane = pane;
    }

    /// An agent is shown when no `!exclusion` and every `~fuzzy` term allow
    /// it, and either every plain term matches its metadata or its
    /// conversation matches them all. Rank: conversation score plus
    /// [`META_BONUS`] for a metadata match; ties keep urgency order.
    fn refilter(&mut self, keep: Option<&str>) {
        let mut ranked: Vec<(f64, usize)> = (0..self.agents.len())
            .filter_map(|i| {
                let meta = self.filter.meta(&self.haystacks[i]);
                let hit = self.hit(&self.agents[i]);
                let shown = meta.allowed && (meta.matched || hit.is_some());
                let bonus = if meta.matched { META_BONUS } else { 0.0 };
                shown.then(|| (bonus + hit.map_or(0.0, |h| h.score), i))
            })
            .collect();
        // Stable: equal scores keep urgency order.
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
        self.visible = ranked.into_iter().map(|(_, i)| i).collect();
        let position = keep.and_then(|pane| {
            self.visible
                .iter()
                .position(|&i| self.agents[i].pane == pane)
        });
        let selected = match (position, self.visible.is_empty()) {
            (_, true) => None,
            (Some(p), false) => Some(p),
            (None, false) => Some(0),
        };
        self.list.select(selected);
    }

    fn edit_query(&mut self, edit: impl FnOnce(&mut String)) {
        let before = self.query.clone();
        edit(&mut self.query);
        if self.query == before {
            return;
        }
        self.filter = Filter::parse(&self.query);
        // Hits belong to the old query; metadata matches show at once.
        self.hits.clear();
        self.schedule(Instant::now() + DEBOUNCE);
        // Typing jumps to the best (first) match, as fzf does.
        self.follow = true;
        self.refilter(None);
    }

    fn move_by(&mut self, delta: isize) {
        let Some(current) = self.list.selected() else {
            return;
        };
        let last = self.visible.len().saturating_sub(1);
        self.list
            .select(Some(current.saturating_add_signed(delta).min(last)));
        self.follow = false;
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.help {
            self.help = false;
            // ^c still quits; any other key just closes the help.
            let quit = ctrl && key.code == KeyCode::Char('c');
            return if quit { Action::Quit } else { Action::None };
        }
        match key.code {
            KeyCode::Char('c' | 'g' | 'q') if ctrl => Action::Quit,
            KeyCode::Char('r') if ctrl => Action::Refresh,
            KeyCode::Enter => self
                .selected_pane()
                .map_or(Action::None, |p| Action::Jump(p.to_owned())),
            KeyCode::Up => self.step(-1),
            KeyCode::Char('p' | 'k') if ctrl => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::Char('n' | 'j') if ctrl => self.step(1),
            KeyCode::PageUp => self.step(-(PAGE as isize)),
            KeyCode::PageDown => self.step(PAGE as isize),
            KeyCode::Char('u') if ctrl => self.edit(String::clear),
            KeyCode::Char('w') if ctrl => self.edit(delete_word),
            KeyCode::Backspace => self.edit(|q| {
                q.pop();
            }),
            _ if ctrl || key.modifiers.contains(KeyModifiers::ALT) => Action::None,
            _ => match self.mode {
                Mode::Filter => self.on_filter_key(key.code),
                Mode::Agents => self.on_agents_key(key.code),
            },
        }
    }

    fn on_filter_key(&mut self, code: KeyCode) -> Action {
        match code {
            // Esc with nothing typed quits straight away, as it always has.
            KeyCode::Esc if self.query.is_empty() => Action::Quit,
            KeyCode::Esc | KeyCode::Tab => self.set_mode(Mode::Agents),
            KeyCode::Char(c) => self.edit(|q| q.push(c)),
            _ => Action::None,
        }
    }

    fn on_agents_key(&mut self, code: KeyCode) -> Action {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('/') => self.set_mode(Mode::Filter),
            KeyCode::Char('j') => self.step(1),
            KeyCode::Char('k') => self.step(-1),
            KeyCode::Char('g') | KeyCode::Home => self.step(isize::MIN),
            KeyCode::Char('G') | KeyCode::End => self.step(isize::MAX),
            KeyCode::Char('r') => Action::Refresh,
            KeyCode::Char('p') => {
                self.preview_choice = Some(!self.show_preview());
                Action::None
            }
            KeyCode::Char('?') => {
                self.help = true;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn set_mode(&mut self, mode: Mode) -> Action {
        self.mode = mode;
        Action::None
    }

    fn step(&mut self, delta: isize) -> Action {
        self.move_by(delta);
        Action::None
    }

    fn edit(&mut self, edit: impl FnOnce(&mut String)) -> Action {
        self.edit_query(edit);
        Action::None
    }
}

/// Readline ctrl-w: drop trailing whitespace, then the word before it.
fn delete_word(query: &mut String) {
    let trimmed = query.trim_end().len();
    query.truncate(trimmed);
    let start = query.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    query.truncate(start);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::model::{Kind, State};
    use mnemoloom::index::snippet::Snippet;

    fn agent(pane: &str, state: State, name: &str) -> Agent {
        let mut a = Agent::new(pane.into(), "s".into(), Kind::Claude);
        a.state = state;
        a.name = name.into();
        a
    }

    fn sample() -> Vec<Agent> {
        vec![
            agent("a:1.1", State::Input, "deploy fix"),
            agent("b:1.1", State::Busy, "write tests"),
            agent("c:1.1", State::Idle, "deploy docs"),
        ]
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn typing_filters_in_order() {
        let mut app = App::new(sample());
        assert_eq!(app.selected_pane(), Some("a:1.1"));
        type_str(&mut app, "deploy");
        assert_eq!(app.visible, vec![0, 2]);
        type_str(&mut app, " idle");
        assert_eq!(app.visible, vec![2]);
        assert_eq!(app.selected_pane(), Some("c:1.1"));
        app.on_key(ctrl('w'));
        assert_eq!(app.query, "deploy ");
        app.on_key(ctrl('u'));
        assert_eq!(app.visible.len(), 3);
        type_str(&mut app, "zzz");
        assert_eq!(app.selected_pane(), None);
        assert_eq!(app.on_key(key(KeyCode::Enter)), Action::None);
    }

    #[test]
    fn navigation_and_actions() {
        let mut app = App::new(sample());
        app.on_key(key(KeyCode::Down));
        app.on_key(ctrl('n'));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected_pane(), Some("c:1.1"));
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(app.selected_pane(), Some("a:1.1"));
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Action::Jump("a:1.1".into())
        );
        assert_eq!(app.on_key(ctrl('r')), Action::Refresh);
        assert_eq!(app.on_key(key(KeyCode::Esc)), Action::Quit);
        assert_eq!(app.on_key(ctrl('c')), Action::Quit);
    }

    #[test]
    fn rescan_keeps_query_and_selection() {
        let mut app = App::new(sample());
        type_str(&mut app, "deploy");
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected_pane(), Some("c:1.1"));
        let mut next = sample();
        next.insert(0, agent("z:1.1", State::Input, "deploy new"));
        app.set_agents(next);
        assert_eq!(app.query, "deploy");
        assert_eq!(app.visible.len(), 3);
        assert_eq!(app.selected_pane(), Some("c:1.1"));
        app.set_agents(vec![agent("q:1.1", State::Idle, "deploy q")]);
        assert_eq!(app.selected_pane(), Some("q:1.1"));
    }

    #[test]
    fn filter_mode_is_modal() {
        let mut app = App::new(sample());
        assert_eq!(app.mode, Mode::Filter);
        type_str(&mut app, "dep");
        assert_eq!(app.query, "dep");
        // Esc with a query returns to the list, keeping the filter.
        assert_eq!(app.on_key(key(KeyCode::Esc)), Action::None);
        assert_eq!(app.mode, Mode::Agents);
        assert_eq!(app.visible, vec![0, 2]);
        // Letters are commands now, not text.
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected_pane(), Some("c:1.1"));
        app.on_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected_pane(), Some("a:1.1"));
        assert_eq!(app.query, "dep");
        app.on_key(key(KeyCode::Char('/')));
        assert_eq!(app.mode, Mode::Filter);
        type_str(&mut app, "q");
        assert_eq!(app.query, "depq");
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.mode, Mode::Agents);
        assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::Quit);
    }

    #[test]
    fn agents_mode_commands() {
        let mut app = App::new(sample());
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Char('G')));
        assert_eq!(app.selected_pane(), Some("c:1.1"));
        app.on_key(key(KeyCode::Char('g')));
        assert_eq!(app.selected_pane(), Some("a:1.1"));
        assert_eq!(app.on_key(key(KeyCode::Char('r'))), Action::Refresh);
        app.on_key(key(KeyCode::Char('p')));
        assert_eq!(app.preview_choice, Some(false));
        app.on_key(key(KeyCode::Char('?')));
        assert!(app.help);
        // Any key closes help and is otherwise ignored.
        assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::None);
        assert!(!app.help);
        assert!(app.query.is_empty());
        assert_eq!(app.on_key(key(KeyCode::Esc)), Action::Quit);
    }

    #[test]
    fn ctrl_c_quits_from_help() {
        let mut app = App::new(sample());
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.on_key(ctrl('c')), Action::Quit);
    }

    #[test]
    fn preview_choice_survives_resizes() {
        let mut app = App::new(sample());
        app.on_key(key(KeyCode::Tab));
        assert!(app.show_preview());
        // Hidden while wide stays hidden after narrowing and widening again.
        app.on_key(key(KeyCode::Char('p')));
        for wide in [false, true] {
            app.wide = wide;
            assert!(!app.show_preview());
        }
        // Shown while narrow stays shown when wide.
        let mut app = App::new(sample());
        app.on_key(key(KeyCode::Tab));
        app.wide = false;
        assert!(!app.show_preview());
        app.on_key(key(KeyCode::Char('p')));
        for wide in [true, false] {
            app.wide = wide;
            assert!(app.show_preview());
        }
    }

    fn hit(score: f64, text: &str) -> Hit {
        Hit {
            score,
            snippet: Snippet {
                text: text.into(),
                highlights: Vec::new(),
            },
        }
    }

    /// `sample()` with session ids, plus one agent without.
    fn with_sessions() -> App {
        let mut agents = sample();
        for (a, id) in agents.iter_mut().zip(["A-1", "b-2", "c-3"]) {
            a.session_id = id.into();
        }
        agents.push(agent("d:1.1", State::Idle, "no session"));
        let mut app = App::new(agents);
        app.set_index_ready(true);
        app
    }

    fn later() -> Instant {
        Instant::now() + DEBOUNCE
    }

    #[test]
    fn conversation_search_is_debounced() {
        let mut app = with_sessions();
        // Nothing to ask without plain terms.
        assert_eq!(app.search_due(later()), None);
        type_str(&mut app, "rocket");
        assert_eq!(app.search_due(Instant::now()), None);
        let req = app.search_due(later()).unwrap();
        assert_eq!(req.conversation.query, "rocket");
        assert_eq!(req.sessions, vec!["A-1", "b-2", "c-3"]);
        // Sent once.
        assert_eq!(app.search_due(later()), None);
        // Each keystroke restarts the wait and supersedes the request.
        type_str(&mut app, " x");
        let next = app.search_due(later()).unwrap();
        assert!(next.seq > req.seq);
        assert_eq!(next.conversation.query, "rocket x");
        // Fuzzy-only and exclusion-only queries stay on metadata.
        app.on_key(ctrl('u'));
        type_str(&mut app, "~dpl !busy");
        assert_eq!(app.search_due(later()), None);
    }

    #[test]
    fn no_search_until_the_index_is_ready() {
        let mut app = App::new(sample());
        app.agents[0].session_id = "a-1".into();
        type_str(&mut app, "rocket");
        assert_eq!(app.search_due(later()), None);
        app.set_index_ready(true);
        assert!(app.search_due(Instant::now()).is_some());
    }

    #[test]
    fn hits_add_conversation_matches_and_rank() {
        let mut app = with_sessions();
        type_str(&mut app, "rocket");
        assert!(app.visible.is_empty());
        let req = app.search_due(later()).unwrap();
        let hits = Hits::from([
            ("b-2".into(), hit(1.0, "rocket")),
            ("c-3".into(), hit(3.0, "rocket")),
        ]);
        assert!(app.set_hits(req.seq, hits));
        // Best score first; the selection follows it.
        assert_eq!(app.visible, vec![2, 1]);
        assert_eq!(app.selected_pane(), Some("c:1.1"));
        assert_eq!(app.hit(&app.agents[1]).unwrap().score, 1.0);
        assert!(app.hit(&app.agents[3]).is_none());
        app.on_key(ctrl('u'));
        type_str(&mut app, "deploy");
        let req = app.search_due(later()).unwrap();
        // Hits are keyed by lowercase session id ("A-1" here).
        let hits = Hits::from([
            ("a-1".into(), hit(9.0, "deploy")),
            ("b-2".into(), hit(2.0, "deploy")),
        ]);
        assert!(app.set_hits(req.seq, hits));
        // Metadata matches (a, c) list first, the higher score leading;
        // then conversation-only (b).
        assert_eq!(app.visible, vec![0, 2, 1]);
    }

    #[test]
    fn stale_hits_are_discarded() {
        let mut app = with_sessions();
        type_str(&mut app, "rocket");
        let req = app.search_due(later()).unwrap();
        type_str(&mut app, "s");
        let hits = Hits::from([("b-2".into(), hit(1.0, "rocket"))]);
        assert!(!app.set_hits(req.seq, hits.clone()));
        assert!(app.visible.is_empty());
        // A rescan supersedes a request too, but keeps current hits.
        let req = app.search_due(later()).unwrap();
        assert!(app.set_hits(req.seq, hits.clone()));
        assert_eq!(app.visible, vec![1]);
        let agents = app.agents.clone();
        app.set_agents(agents);
        assert_eq!(app.visible, vec![1]);
        assert!(!app.set_hits(req.seq, Hits::new()));
        assert_eq!(app.visible, vec![1]);
        // Editing drops hits at once.
        app.on_key(key(KeyCode::Backspace));
        assert!(app.visible.is_empty());
        // So does losing the index.
        type_str(&mut app, "s");
        let req = app.search_due(later()).unwrap();
        assert!(app.set_hits(req.seq, hits));
        app.set_index_ready(false);
        assert!(app.visible.is_empty());
    }

    #[test]
    fn exclusions_and_fuzzy_terms_gate_hits() {
        let mut app = with_sessions();
        type_str(&mut app, "rocket !idle");
        let req = app.search_due(later()).unwrap();
        assert_eq!(req.conversation.exclude, "idle");
        let hits = Hits::from([("b-2".into(), hit(1.0, "x")), ("c-3".into(), hit(2.0, "x"))]);
        assert!(app.set_hits(req.seq, hits.clone()));
        // c is idle: its metadata excludes it whatever the conversation says.
        assert_eq!(app.visible, vec![1]);
        app.on_key(ctrl('u'));
        type_str(&mut app, "rocket ~dpdc");
        let req = app.search_due(later()).unwrap();
        assert!(app.set_hits(req.seq, hits));
        assert_eq!(app.visible, vec![2]);
    }

    #[test]
    fn hits_keep_a_moved_selection() {
        let mut app = with_sessions();
        type_str(&mut app, "deploy");
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected_pane(), Some("c:1.1"));
        let req = app.search_due(later()).unwrap();
        let hits = Hits::from([("b-2".into(), hit(5.0, "x"))]);
        assert!(app.set_hits(req.seq, hits));
        assert_eq!(app.visible, vec![0, 2, 1]);
        assert_eq!(app.selected_pane(), Some("c:1.1"));
    }

    #[test]
    fn delete_word_like_readline() {
        let mut q = String::from("foo bar  ");
        delete_word(&mut q);
        assert_eq!(q, "foo ");
        delete_word(&mut q);
        assert_eq!(q, "");
    }
}
