//! Picker state and key handling, independent of the terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use ratatui::widgets::ListState;

use super::{ansi, fuzzy};
use crate::mux::model::Agent;
use crate::mux::render;

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
    /// Plain row text per agent, the filter haystack.
    rows: Vec<String>,
    pub query: String,
    /// Indices into `agents` that match `query`, in urgency order.
    pub visible: Vec<usize>,
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
            rows: Vec::new(),
            query: String::new(),
            visible: Vec::new(),
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
        self.rows = agents.iter().map(|a| render::row(a, false)).collect();
        self.agents = agents;
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

    fn refilter(&mut self, keep: Option<&str>) {
        self.visible = (0..self.agents.len())
            .filter(|&i| fuzzy::matches(&self.query, &self.rows[i]))
            .collect();
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
        edit(&mut self.query);
        // Typing jumps to the best (first) match, as fzf does.
        self.refilter(None);
    }

    fn move_by(&mut self, delta: isize) {
        let Some(current) = self.list.selected() else {
            return;
        };
        let last = self.visible.len().saturating_sub(1);
        self.list
            .select(Some(current.saturating_add_signed(delta).min(last)));
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

    #[test]
    fn delete_word_like_readline() {
        let mut q = String::from("foo bar  ");
        delete_word(&mut q);
        assert_eq!(q, "foo ");
        delete_word(&mut q);
        assert_eq!(q, "");
    }
}
