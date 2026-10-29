//! The menu as a pure state machine: [`State::handle`] maps a [`Key`] to a
//! new screen or a [`Step`] telling the caller what to run. Rendering lives
//! in `ui`, terminal IO in `tui`.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::forms::{
    ContinueForm, Input, QUERY_DEFAULT, RolloverOptions, SMART_TRIM_DEFAULT, TrimForm, TrimOptions,
};
use super::session::SessionView;
use crate::cli::StartScreen;
use crate::session::Agent;

/// Input the reducer understands (terminal-independent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Esc,
    Backspace,
    Char(char),
    /// Ctrl-C: leave immediately.
    Interrupt,
    Other,
}

impl From<KeyEvent> for Key {
    fn from(k: KeyEvent) -> Key {
        if k.modifiers.contains(KeyModifiers::CONTROL) {
            return match k.code {
                KeyCode::Char('c') => Key::Interrupt,
                _ => Key::Other,
            };
        }
        match k.code {
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Enter => Key::Enter,
            KeyCode::Esc => Key::Esc,
            KeyCode::Backspace | KeyCode::Delete => Key::Backspace,
            KeyCode::Char(c) => Key::Char(c),
            _ => Key::Other,
        }
    }
}

impl Key {
    /// Key presses only (no repeats/releases on platforms that report them).
    pub fn from_event(k: KeyEvent) -> Option<Key> {
        (k.kind != KeyEventKind::Release).then(|| Key::from(k))
    }
}

/// Main action menu entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Path,
    Copy,
    Export,
    Query,
    ResumeMenu,
}

pub const ACTIONS: [(MenuItem, &str); 5] = [
    (MenuItem::Path, "Show session file path"),
    (MenuItem::Copy, "Copy session file"),
    (MenuItem::Export, "Export to text file (.txt)"),
    (MenuItem::Query, "Query this session..."),
    (MenuItem::ResumeMenu, "Resume/trim session..."),
];

/// Resume / trim submenu entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeItem {
    Resume,
    Clone,
    Trim,
    SmartTrim,
    Rollover,
}

pub const RESUME_SUBMENU: [(ResumeItem, &str); 5] = [
    (ResumeItem::Resume, "Resume as-is"),
    (ResumeItem::Clone, "Clone session and resume clone"),
    (ResumeItem::Trim, "Trim + resume..."),
    (ResumeItem::SmartTrim, "Smart trim + resume..."),
    (
        ResumeItem::Rollover,
        "Rollover: handoff work to fresh session...",
    ),
];

pub const TRIM_SUBMENU: [(ResumeItem, &str); 2] = [
    (ResumeItem::Trim, "Trim + resume..."),
    (ResumeItem::SmartTrim, "Smart trim + resume..."),
];

/// Read-only entries for agents mnemoloom can't rewrite (pi, opencode):
/// no copy; the resume submenu only resumes ([`resume_items`]).
const READ_ONLY_ACTIONS: [(MenuItem, &str); 4] = [
    (MenuItem::Path, "Show session file path"),
    (MenuItem::Export, "Export to text file (.txt)"),
    (MenuItem::Query, "Query this session..."),
    (MenuItem::ResumeMenu, "Resume session..."),
];

/// Action menu entries for a session (sub-agents: non-launch only).
pub fn actions(s: &SessionView) -> &'static [(MenuItem, &'static str)] {
    match (s.agent.is_rewritable(), s.is_sidechain) {
        (true, true) => &ACTIONS[..4],
        (true, false) => &ACTIONS,
        (false, true) => &READ_ONLY_ACTIONS[..3],
        (false, false) => &READ_ONLY_ACTIONS,
    }
}

/// Resume submenu entries for `agent` (pi / opencode: resume only).
pub fn resume_items(agent: Agent) -> &'static [(ResumeItem, &'static str)] {
    if agent.is_rewritable() {
        &RESUME_SUBMENU
    } else {
        &RESUME_SUBMENU[..1]
    }
}

/// Non-launch actions with a result/prompt screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonLaunchKind {
    Path,
    Copy,
    Export,
}

impl NonLaunchKind {
    /// Badge text (`action.toUpperCase()`).
    pub fn badge(self) -> &'static str {
        match self {
            NonLaunchKind::Path => "PATH",
            NonLaunchKind::Copy => "COPY",
            NonLaunchKind::Export => "EXPORT",
        }
    }
}

/// Copy / export destination prompt, or the path display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonLaunch {
    pub kind: NonLaunchKind,
    pub dest: String,
    pub error: Option<String>,
}

/// Query question prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryForm {
    pub input: Input,
    pub error: Option<String>,
}

/// Data for the trim-confirm dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrimInfo {
    pub new_session_id: Option<String>,
    pub original_session_id: Option<String>,
    pub lines_trimmed: usize,
    pub tokens_saved: usize,
    pub output_file: String,
    pub nothing_to_trim: bool,
}

impl TrimInfo {
    /// The dialog's options.
    pub fn items(&self) -> [(TrimChoice, &'static str); 2] {
        if self.nothing_to_trim {
            [
                (TrimChoice::Resume, "Resume original session"),
                (TrimChoice::Back, "Back to menu"),
            ]
        } else {
            [
                (TrimChoice::Resume, "Resume trimmed session"),
                (TrimChoice::Delete, "Delete session file & exit"),
            ]
        }
    }
}

/// Trim-confirm result (`resume` / `delete` / `back` / `cancel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimChoice {
    Resume,
    Delete,
    Back,
    /// Esc: keep the file, do nothing.
    Cancel,
}

/// Data for the directory-confirm dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirInfo {
    pub current_dir: String,
    pub session_dir: String,
}

/// Directory-confirm result (`yes` / `no` / `cancel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirChoice {
    Yes,
    No,
    Cancel,
}

pub const DIR_ITEMS: [(DirChoice, &str); 2] = [
    (DirChoice::Yes, "Yes, change directory and proceed"),
    (DirChoice::No, "No, proceed without changing directory"),
];

/// Where the trim forms return to on Esc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimSource {
    Resume,
    TrimMenu,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// Candidate picker (more than one latest session).
    Results,
    Action,
    Resume,
    TrimMenu,
    Trim(TrimForm),
    SmartTrim(Input),
    Lineage,
    Continue(ContinueForm),
    Query(QueryForm),
    NonLaunch(NonLaunch),
    TrimConfirm(TrimInfo),
    DirConfirm(DirInfo),
}

/// What to do once the menu is done with the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Path,
    Copy { dest: String },
    Export { dest: PathBuf },
    Query { question: String },
    Resume,
    Clone,
    Trim(TrimOptions),
    SmartTrim { instructions: String },
    Rollover(RolloverOptions),
    TrimConfirm(TrimChoice),
    DirConfirm(DirChoice),
}

/// Reducer outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Stay,
    Quit,
    /// Ctrl-C outside a dialog: leave with exit status 130.
    Interrupt,
    /// Leave the UI and run `action` on `sessions[current]`.
    Run(Action),
}

/// Full UI state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub screen: Screen,
    /// Highlighted row of list screens.
    pub cursor: usize,
    /// Index of the session being acted on.
    pub current: usize,
    pub start: StartScreen,
    /// More than one candidate (the results picker is in play).
    pub multi: bool,
    pub trim_source: Option<TrimSource>,
    /// The `?` key-help overlay is open.
    pub help: bool,
}

/// Outcome of a key on a list screen.
enum ListKey {
    None,
    Select(usize),
    Back,
}

/// Arrows/jk move with wrap-around, digits jump, Enter selects, Esc/q back.
fn list_key(cursor: &mut usize, len: usize, key: Key) -> ListKey {
    match key {
        Key::Up | Key::Char('k') => *cursor = if *cursor == 0 { len - 1 } else { *cursor - 1 },
        Key::Down | Key::Char('j') => *cursor = if *cursor + 1 >= len { 0 } else { *cursor + 1 },
        Key::Char(c @ '1'..='9') => {
            let n = c as usize - '0' as usize;
            if n <= len {
                *cursor = n - 1;
            }
        }
        Key::Enter => return ListKey::Select(*cursor),
        Key::Esc | Key::Char('q') => return ListKey::Back,
        _ => {}
    }
    ListKey::None
}

impl State {
    /// Initial state for `count` candidate sessions.
    pub fn new(start: StartScreen, count: usize) -> State {
        let multi = count > 1;
        let mut s = State {
            screen: Screen::Results,
            cursor: 0,
            current: 0,
            start,
            multi,
            trim_source: None,
            help: false,
        };
        if !multi {
            s.screen = s.start_screen();
        }
        s
    }

    /// A standalone dialog (trim-confirm / dir-confirm).
    pub fn dialog(screen: Screen) -> State {
        State {
            screen,
            ..State::new(StartScreen::Action, 1)
        }
    }

    fn start_screen(&self) -> Screen {
        match self.start {
            StartScreen::Action => Screen::Action,
            StartScreen::Resume => Screen::Resume,
            StartScreen::TrimMenu => Screen::TrimMenu,
            StartScreen::SmartTrimForm => Screen::SmartTrim(Input::default()),
            StartScreen::Lineage => Screen::Lineage,
            StartScreen::Query => Screen::Query(QueryForm::default()),
        }
    }

    fn go(&mut self, screen: Screen) -> Step {
        self.screen = screen;
        self.cursor = 0;
        Step::Stay
    }

    /// The screen is editing text, so `?` and `q` are typed, not keys.
    pub fn text_entry(&self) -> bool {
        match &self.screen {
            Screen::Trim(_) | Screen::SmartTrim(_) | Screen::Continue(_) => true,
            Screen::Query(q) => q.error.is_none(),
            Screen::NonLaunch(nl) => nl.kind != NonLaunchKind::Path && nl.error.is_none(),
            _ => false,
        }
    }

    /// Apply `key`. `sessions` is the candidate list (read-only).
    pub fn handle(&mut self, key: Key, sessions: &[SessionView]) -> Step {
        if key == Key::Interrupt {
            return match self.screen {
                Screen::TrimConfirm(_) => Step::Run(Action::TrimConfirm(TrimChoice::Cancel)),
                Screen::DirConfirm(_) => Step::Run(Action::DirConfirm(DirChoice::Cancel)),
                _ => Step::Interrupt,
            };
        }
        if self.help {
            self.help = false;
            return Step::Stay;
        }
        if key == Key::Char('?') && !self.text_entry() {
            self.help = true;
            return Step::Stay;
        }
        let Some(session) = sessions.get(self.current) else {
            return self.handle_dialog(key);
        };
        match self.screen {
            Screen::Results => self.on_results(key, sessions.len()),
            Screen::Action => self.on_action(key, session),
            Screen::Resume => self.on_resume(key, session),
            Screen::TrimMenu => self.on_trim_menu(key),
            Screen::Trim(_) => self.on_trim(key),
            Screen::SmartTrim(_) => self.on_smart_trim(key),
            Screen::Lineage => self.on_lineage(key, session),
            Screen::Continue(_) => self.on_continue(key),
            Screen::Query(_) => self.on_query(key),
            Screen::NonLaunch(_) => self.on_nonlaunch(key, session),
            Screen::TrimConfirm(_) | Screen::DirConfirm(_) => self.handle_dialog(key),
        }
    }

    fn on_results(&mut self, key: Key, len: usize) -> Step {
        let select = match key {
            Key::Up | Key::Char('k') => {
                self.cursor = self.cursor.saturating_sub(1);
                None
            }
            Key::Down | Key::Char('j') => {
                self.cursor = (self.cursor + 1).min(len - 1);
                None
            }
            Key::Char(c @ '1'..='9') if (c as usize - '0' as usize) <= len => {
                Some(c as usize - '1' as usize)
            }
            Key::Enter => Some(self.cursor),
            Key::Esc | Key::Char('q') => return Step::Quit,
            _ => None,
        };
        if let Some(i) = select {
            self.current = i;
            let screen = self.start_screen();
            return self.go(screen);
        }
        Step::Stay
    }

    fn on_action(&mut self, key: Key, session: &SessionView) -> Step {
        let items = actions(session);
        match list_key(&mut self.cursor, items.len(), key) {
            ListKey::None => Step::Stay,
            ListKey::Back if self.multi => self.go(Screen::Results),
            ListKey::Back => Step::Quit,
            ListKey::Select(i) => match items[i].0 {
                MenuItem::Path => self.nonlaunch(NonLaunchKind::Path),
                MenuItem::Copy => self.nonlaunch(NonLaunchKind::Copy),
                MenuItem::Export => self.nonlaunch(NonLaunchKind::Export),
                MenuItem::Query => self.go(Screen::Query(QueryForm::default())),
                MenuItem::ResumeMenu => self.go(Screen::Resume),
            },
        }
    }

    fn nonlaunch(&mut self, kind: NonLaunchKind) -> Step {
        self.go(Screen::NonLaunch(NonLaunch {
            kind,
            dest: String::new(),
            error: None,
        }))
    }

    fn on_resume(&mut self, key: Key, session: &SessionView) -> Step {
        let items = resume_items(session.agent);
        match list_key(&mut self.cursor, items.len(), key) {
            ListKey::None => Step::Stay,
            ListKey::Back => match (self.start, self.multi) {
                (StartScreen::Resume, true) => self.go(Screen::Results),
                (StartScreen::Resume, false) => Step::Quit,
                _ => self.go(Screen::Action),
            },
            ListKey::Select(i) => self.choose_resume_item(items[i].0, TrimSource::Resume),
        }
    }

    fn on_trim_menu(&mut self, key: Key) -> Step {
        match list_key(&mut self.cursor, TRIM_SUBMENU.len(), key) {
            ListKey::None => Step::Stay,
            ListKey::Back if self.multi => self.go(Screen::Results),
            ListKey::Back => Step::Quit,
            ListKey::Select(i) => self.choose_resume_item(TRIM_SUBMENU[i].0, TrimSource::TrimMenu),
        }
    }

    fn choose_resume_item(&mut self, item: ResumeItem, source: TrimSource) -> Step {
        match item {
            ResumeItem::Resume => Step::Run(Action::Resume),
            ResumeItem::Clone => Step::Run(Action::Clone),
            ResumeItem::Trim => {
                self.trim_source = Some(source);
                self.go(Screen::Trim(TrimForm::default()))
            }
            ResumeItem::SmartTrim => {
                self.trim_source = Some(source);
                self.go(Screen::SmartTrim(Input::default()))
            }
            ResumeItem::Rollover => self.go(Screen::Lineage),
        }
    }

    /// Esc from the trim forms: back to the trim source, else resume.
    fn trim_back(&mut self) -> Step {
        match self.trim_source {
            Some(TrimSource::TrimMenu) => self.go(Screen::TrimMenu),
            _ => self.go(Screen::Resume),
        }
    }

    fn on_trim(&mut self, key: Key) -> Step {
        let Screen::Trim(form) = &mut self.screen else {
            return Step::Stay;
        };
        match key {
            Key::Esc => return self.trim_back(),
            Key::Down => form.next_field(),
            Key::Up => form.prev_field(),
            Key::Enter if form.field == super::forms::TrimField::Assistant => {
                return Step::Run(Action::Trim(form.options()));
            }
            Key::Enter => form.next_field(),
            Key::Backspace => form.backspace(),
            Key::Char(c) => form.push(c),
            _ => {}
        }
        Step::Stay
    }

    fn on_smart_trim(&mut self, key: Key) -> Step {
        let Screen::SmartTrim(input) = &mut self.screen else {
            return Step::Stay;
        };
        match key {
            Key::Esc => return self.trim_back(),
            Key::Enter => {
                return Step::Run(Action::SmartTrim {
                    instructions: input.value_or(SMART_TRIM_DEFAULT),
                });
            }
            Key::Backspace => input.backspace(),
            Key::Char(c) => input.push(c),
            _ => {}
        }
        Step::Stay
    }

    fn on_lineage(&mut self, key: Key, session: &SessionView) -> Step {
        if session.details.is_none() {
            return Step::Stay;
        }
        match key {
            Key::Esc | Key::Char('q') if self.start == StartScreen::Lineage => Step::Quit,
            Key::Esc | Key::Char('q') => self.go(Screen::Resume),
            Key::Enter => self.go(Screen::Continue(ContinueForm::new(session.agent))),
            _ => Step::Stay,
        }
    }

    fn on_continue(&mut self, key: Key) -> Step {
        let Screen::Continue(form) = &mut self.screen else {
            return Step::Stay;
        };
        match key {
            Key::Esc => return self.go(Screen::Lineage),
            Key::Down => {
                form.field = form
                    .next_field()
                    .unwrap_or(super::forms::ContinueField::Agent)
            }
            Key::Up => form.field = form.prev_field(),
            Key::Enter => match form.next_field() {
                Some(f) => form.field = f,
                None => return Step::Run(Action::Rollover(form.options())),
            },
            Key::Backspace => form.backspace(),
            Key::Char(c) => form.push(c),
            _ => {}
        }
        Step::Stay
    }

    fn query_back(&mut self) -> Step {
        if self.start == StartScreen::Query {
            Step::Quit
        } else {
            self.go(Screen::Action)
        }
    }

    fn on_query(&mut self, key: Key) -> Step {
        let Screen::Query(q) = &mut self.screen else {
            return Step::Stay;
        };
        if q.error.is_some() {
            return match key {
                Key::Esc | Key::Enter | Key::Char('q') => self.query_back(),
                _ => Step::Stay,
            };
        }
        match key {
            Key::Esc => return self.query_back(),
            Key::Enter => {
                let question = q.input.value_or(QUERY_DEFAULT).trim().to_string();
                if question.is_empty() {
                    q.error = Some("Query cannot be empty".into());
                    return Step::Stay;
                }
                return Step::Run(Action::Query { question });
            }
            Key::Backspace => q.input.backspace(),
            Key::Char(c) => q.input.push(c),
            _ => {}
        }
        Step::Stay
    }

    fn on_nonlaunch(&mut self, key: Key, session: &SessionView) -> Step {
        let Screen::NonLaunch(nl) = &mut self.screen else {
            return Step::Stay;
        };
        if nl.kind == NonLaunchKind::Path || nl.error.is_some() {
            return match key {
                Key::Esc | Key::Char('q') => self.go(Screen::Action),
                Key::Enter if nl.error.is_none() => Step::Run(Action::Path),
                Key::Enter => Step::Quit,
                _ => Step::Stay,
            };
        }
        match key {
            Key::Esc => return self.go(Screen::Action),
            Key::Enter => {
                let dest = nl.dest.trim().to_string();
                return match (nl.kind, dest.is_empty()) {
                    (NonLaunchKind::Export, true) => Step::Run(Action::Export {
                        dest: session.default_export_path.clone(),
                    }),
                    (NonLaunchKind::Export, false) => Step::Run(Action::Export {
                        dest: PathBuf::from(dest),
                    }),
                    (_, true) => {
                        nl.error = Some("Destination required".into());
                        Step::Stay
                    }
                    (_, false) => Step::Run(Action::Copy { dest }),
                };
            }
            Key::Backspace => {
                nl.dest.pop();
            }
            Key::Char(c) => nl.dest.push(c),
            _ => {}
        }
        Step::Stay
    }

    fn handle_dialog(&mut self, key: Key) -> Step {
        match &self.screen {
            Screen::TrimConfirm(info) => {
                let items = info.items();
                match list_key(&mut self.cursor, items.len(), key) {
                    ListKey::None => Step::Stay,
                    ListKey::Back => Step::Run(Action::TrimConfirm(TrimChoice::Cancel)),
                    ListKey::Select(i) => Step::Run(Action::TrimConfirm(items[i].0)),
                }
            }
            Screen::DirConfirm(_) => match list_key(&mut self.cursor, DIR_ITEMS.len(), key) {
                ListKey::None => Step::Stay,
                ListKey::Back => Step::Run(Action::DirConfirm(DirChoice::Cancel)),
                ListKey::Select(i) => Step::Run(Action::DirConfirm(DIR_ITEMS[i].0)),
            },
            _ => Step::Quit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::forms::{RolloverType, TrimField};
    use crate::menu::session::fixture::view;
    use crate::session::Agent;

    fn one() -> Vec<SessionView> {
        vec![view(Agent::Claude, "c1")]
    }

    fn two() -> Vec<SessionView> {
        vec![view(Agent::Claude, "c1"), view(Agent::Codex, "x2")]
    }

    fn keys(state: &mut State, sessions: &[SessionView], ks: &[Key]) -> Step {
        let mut last = Step::Stay;
        for &k in ks {
            last = state.handle(k, sessions);
        }
        last
    }

    fn typed(s: &str) -> Vec<Key> {
        s.chars().map(Key::Char).collect()
    }

    #[test]
    fn key_conversion() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        assert_eq!(
            Key::from(KeyEvent::from(KeyCode::Char('j'))),
            Key::Char('j')
        );
        assert_eq!(
            Key::from(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Key::Interrupt
        );
        assert_eq!(Key::from(KeyEvent::from(KeyCode::Delete)), Key::Backspace);
        assert_eq!(Key::from(KeyEvent::from(KeyCode::Tab)), Key::Other);
    }

    #[test]
    fn initial_screens() {
        assert_eq!(State::new(StartScreen::Action, 2).screen, Screen::Results);
        assert_eq!(State::new(StartScreen::Action, 1).screen, Screen::Action);
        assert_eq!(State::new(StartScreen::Resume, 1).screen, Screen::Resume);
        assert_eq!(
            State::new(StartScreen::TrimMenu, 1).screen,
            Screen::TrimMenu
        );
        assert_eq!(State::new(StartScreen::Lineage, 1).screen, Screen::Lineage);
        assert!(matches!(
            State::new(StartScreen::SmartTrimForm, 1).screen,
            Screen::SmartTrim(_)
        ));
        assert!(matches!(
            State::new(StartScreen::Query, 1).screen,
            Screen::Query(_)
        ));
    }

    #[test]
    fn results_picker() {
        let ss = two();
        let mut s = State::new(StartScreen::Action, 2);
        keys(&mut s, &ss, &[Key::Up, Key::Char('j'), Key::Down]);
        assert_eq!(s.cursor, 1, "no wrap on results");
        keys(&mut s, &ss, &[Key::Enter]);
        assert_eq!((s.current, &s.screen), (1, &Screen::Action));
        // Esc on action with several candidates returns to results.
        keys(&mut s, &ss, &[Key::Esc]);
        assert_eq!(s.screen, Screen::Results);
        // Digit selects directly and lands on the start screen.
        let mut s = State::new(StartScreen::Resume, 2);
        keys(&mut s, &ss, &[Key::Char('2')]);
        assert_eq!((s.current, &s.screen), (1, &Screen::Resume));
        assert_eq!(keys(&mut s, &ss, &[Key::Char('9')]), Step::Stay);
        keys(&mut s, &ss, &[Key::Esc]);
        assert_eq!(s.screen, Screen::Results);
        assert_eq!(keys(&mut s, &ss, &[Key::Char('q')]), Step::Quit);
    }

    #[test]
    fn action_menu() {
        let ss = one();
        let mut s = State::new(StartScreen::Action, 1);
        // Wrap-around and digit jump (digit does not select).
        keys(&mut s, &ss, &[Key::Up]);
        assert_eq!(s.cursor, 4);
        keys(&mut s, &ss, &[Key::Char('j')]);
        assert_eq!(s.cursor, 0);
        assert_eq!(keys(&mut s, &ss, &[Key::Char('5')]), Step::Stay);
        assert_eq!(s.cursor, 4);
        keys(&mut s, &ss, &[Key::Enter]);
        assert_eq!(s.screen, Screen::Resume);
        keys(&mut s, &ss, &[Key::Esc]);
        assert_eq!((&s.screen, s.cursor), (&Screen::Action, 0));
        keys(&mut s, &ss, &[Key::Char('4'), Key::Enter]);
        assert!(matches!(s.screen, Screen::Query(_)));
        keys(&mut s, &ss, &[Key::Esc]);
        assert_eq!(s.screen, Screen::Action);
        assert_eq!(keys(&mut s, &ss, &[Key::Esc]), Step::Quit);
        assert_eq!(
            State::new(StartScreen::Action, 1).handle(Key::Interrupt, &ss),
            Step::Interrupt
        );
    }

    #[test]
    fn sidechain_has_no_resume() {
        let mut ss = one();
        ss[0].is_sidechain = true;
        let mut s = State::new(StartScreen::Action, 1);
        keys(&mut s, &ss, &[Key::Up]);
        assert_eq!(s.cursor, 3);
        keys(&mut s, &ss, &[Key::Char('5')]);
        assert_eq!(s.cursor, 3);
        keys(&mut s, &ss, &[Key::Enter]);
        assert!(matches!(s.screen, Screen::Query(_)));
    }

    #[test]
    fn read_only_agents_hide_rewriting_actions() {
        let ss = vec![view(Agent::OpenCode, "ses_1")];
        let labels: Vec<&str> = actions(&ss[0]).iter().map(|a| a.1).collect();
        assert!(!labels.iter().any(|l| l.contains("Copy")), "{labels:?}");
        let mut s = State::new(StartScreen::Action, 1);
        // Last entry is the resume submenu; it offers only "Resume as-is".
        keys(&mut s, &ss, &[Key::Up, Key::Enter]);
        assert_eq!(s.screen, Screen::Resume);
        keys(&mut s, &ss, &[Key::Down]);
        assert_eq!(s.cursor, 0);
        assert_eq!(keys(&mut s, &ss, &[Key::Enter]), Step::Run(Action::Resume));
        assert_eq!(resume_items(Agent::Pi).len(), 1);
        assert_eq!(resume_items(Agent::Codex).len(), RESUME_SUBMENU.len());
    }

    #[test]
    fn nonlaunch_screens() {
        let ss = one();
        let mut s = State::new(StartScreen::Action, 1);
        keys(&mut s, &ss, &[Key::Enter]);
        assert!(matches!(&s.screen, Screen::NonLaunch(n) if n.kind == NonLaunchKind::Path));
        assert_eq!(keys(&mut s, &ss, &[Key::Enter]), Step::Run(Action::Path));
        keys(&mut s, &ss, &[Key::Esc, Key::Char('2'), Key::Enter]);
        // Copy: `q` is text here; empty destination is an error.
        keys(&mut s, &ss, &[Key::Char('q'), Key::Backspace, Key::Enter]);
        assert!(
            matches!(&s.screen, Screen::NonLaunch(n) if n.error.as_deref() == Some("Destination required"))
        );
        assert_eq!(keys(&mut s, &ss, &[Key::Enter]), Step::Quit);
        let mut s = State::new(StartScreen::Action, 1);
        keys(&mut s, &ss, &[Key::Char('2'), Key::Enter]);
        let mut ks = typed("/tmp/out/");
        ks.push(Key::Enter);
        assert_eq!(
            keys(&mut s, &ss, &ks),
            Step::Run(Action::Copy {
                dest: "/tmp/out/".into()
            })
        );
        // Export: blank = default path.
        let mut s = State::new(StartScreen::Action, 1);
        keys(&mut s, &ss, &[Key::Char('3'), Key::Enter]);
        assert_eq!(
            keys(&mut s, &ss, &[Key::Enter]),
            Step::Run(Action::Export {
                dest: ss[0].default_export_path.clone()
            })
        );
        let mut s = State::new(StartScreen::Action, 1);
        keys(&mut s, &ss, &[Key::Char('3'), Key::Enter]);
        let mut ks = typed("x.txt");
        ks.push(Key::Enter);
        assert_eq!(
            keys(&mut s, &ss, &ks),
            Step::Run(Action::Export {
                dest: "x.txt".into()
            })
        );
    }

    #[test]
    fn query_screen() {
        let ss = one();
        let mut s = State::new(StartScreen::Query, 1);
        assert_eq!(
            keys(&mut s, &ss, &[Key::Enter]),
            Step::Run(Action::Query {
                question: QUERY_DEFAULT.into()
            })
        );
        let mut s = State::new(StartScreen::Query, 1);
        let mut ks = typed("why? ");
        ks.push(Key::Enter);
        assert_eq!(
            keys(&mut s, &ss, &ks),
            Step::Run(Action::Query {
                question: "why?".into()
            })
        );
        let mut s = State::new(StartScreen::Query, 1);
        keys(&mut s, &ss, &[Key::Char(' '), Key::Enter]);
        assert!(matches!(&s.screen, Screen::Query(q) if q.error.is_some()));
        assert_eq!(
            keys(&mut s, &ss, &[Key::Esc]),
            Step::Quit,
            "started here: Esc exits"
        );
    }

    #[test]
    fn resume_submenu() {
        let ss = one();
        let mut s = State::new(StartScreen::Resume, 1);
        assert_eq!(keys(&mut s, &ss, &[Key::Enter]), Step::Run(Action::Resume));
        assert_eq!(
            keys(&mut s, &ss, &[Key::Char('2'), Key::Enter]),
            Step::Run(Action::Clone)
        );
        assert_eq!(keys(&mut s, &ss, &[Key::Esc]), Step::Quit);
        let mut s = State::new(StartScreen::Resume, 1);
        keys(&mut s, &ss, &[Key::Char('3'), Key::Enter]);
        assert!(matches!(s.screen, Screen::Trim(_)));
        assert_eq!(s.trim_source, Some(TrimSource::Resume));
        keys(&mut s, &ss, &[Key::Esc]);
        assert_eq!(s.screen, Screen::Resume);
        keys(&mut s, &ss, &[Key::Char('4'), Key::Enter]);
        assert!(matches!(s.screen, Screen::SmartTrim(_)));
        keys(&mut s, &ss, &[Key::Esc, Key::Char('5'), Key::Enter]);
        assert_eq!(s.screen, Screen::Lineage);
    }

    #[test]
    fn trim_form_flow() {
        let ss = one();
        let mut s = State::new(StartScreen::TrimMenu, 1);
        keys(&mut s, &ss, &[Key::Enter]);
        assert_eq!(s.trim_source, Some(TrimSource::TrimMenu));
        let mut ks = typed("bash");
        ks.extend([Key::Enter, Key::Backspace, Key::Backspace, Key::Backspace]);
        ks.extend(typed("2k"));
        ks.extend([Key::Backspace, Key::Up, Key::Down, Key::Enter]);
        keys(&mut s, &ss, &ks);
        assert!(matches!(&s.screen, Screen::Trim(f) if f.field == TrimField::Assistant));
        let mut ks = typed("-3");
        ks.push(Key::Enter);
        assert_eq!(
            keys(&mut s, &ss, &ks),
            Step::Run(Action::Trim(TrimOptions {
                tools: Some("bash".into()),
                threshold: 2,
                trim_assistant: Some(-3)
            }))
        );
        keys(&mut s, &ss, &[Key::Esc]);
        assert_eq!(s.screen, Screen::TrimMenu);
        assert_eq!(keys(&mut s, &ss, &[Key::Esc]), Step::Quit);
        // From trim_menu with several candidates, Esc returns to results.
        let ss2 = two();
        let mut s = State::new(StartScreen::TrimMenu, 2);
        keys(&mut s, &ss2, &[Key::Enter, Key::Esc]);
        assert_eq!(s.screen, Screen::Results);
    }

    #[test]
    fn smart_trim_form() {
        let ss = one();
        let mut s = State::new(StartScreen::SmartTrimForm, 1);
        assert_eq!(
            keys(&mut s, &ss, &[Key::Backspace, Key::Enter]),
            Step::Run(Action::SmartTrim {
                instructions: SMART_TRIM_DEFAULT.into()
            })
        );
        let mut ks = typed("keep diffs");
        ks.push(Key::Enter);
        assert_eq!(
            keys(&mut s, &ss, &ks),
            Step::Run(Action::SmartTrim {
                instructions: "keep diffs".into()
            })
        );
        // No trim source -> back goes to the resume screen.
        keys(&mut s, &ss, &[Key::Esc]);
        assert_eq!(s.screen, Screen::Resume);
    }

    #[test]
    fn lineage_and_rollover_form() {
        let mut ss = one();
        let mut s = State::new(StartScreen::Lineage, 1);
        let details = ss[0].details.take();
        assert_eq!(keys(&mut s, &ss, &[Key::Enter]), Step::Stay);
        assert_eq!(s.screen, Screen::Lineage, "waits for lineage to load");
        ss[0].details = details;
        keys(&mut s, &ss, &[Key::Enter]);
        assert!(matches!(&s.screen, Screen::Continue(f) if f.agent == Agent::Claude));
        // Quick (default): Enter, Enter submits.
        assert_eq!(
            keys(&mut s, &ss, &[Key::Enter, Key::Enter]),
            Step::Run(Action::Rollover(RolloverOptions {
                agent: Agent::Claude,
                rollover_type: RolloverType::Quick,
                prompt: String::new()
            }))
        );
        keys(&mut s, &ss, &[Key::Esc]);
        assert_eq!(s.screen, Screen::Lineage);
        keys(
            &mut s,
            &ss,
            &[
                Key::Enter,
                Key::Char('2'),
                Key::Down,
                Key::Char('2'),
                Key::Enter,
            ],
        );
        let mut ks = typed("focus on q");
        ks.push(Key::Enter);
        assert_eq!(
            keys(&mut s, &ss, &ks),
            Step::Run(Action::Rollover(RolloverOptions {
                agent: Agent::Codex,
                rollover_type: RolloverType::Context,
                prompt: "focus on q".into()
            }))
        );
        // Down wraps to the agent field; Up from agent goes to the prompt.
        keys(&mut s, &ss, &[Key::Down]);
        assert!(
            matches!(&s.screen, Screen::Continue(f) if f.field == crate::menu::forms::ContinueField::Agent)
        );
        keys(&mut s, &ss, &[Key::Esc]);
        assert_eq!(keys(&mut s, &ss, &[Key::Esc]), Step::Quit);
        // Reached from the resume menu, lineage goes back there.
        let mut s = State::new(StartScreen::Resume, 1);
        keys(&mut s, &ss, &[Key::Char('5'), Key::Enter, Key::Esc]);
        assert_eq!(s.screen, Screen::Resume);
    }

    #[test]
    fn help_overlay() {
        let ss = one();
        let mut s = State::new(StartScreen::Action, 1);
        keys(&mut s, &ss, &[Key::Down, Key::Char('?')]);
        assert!(s.help);
        // Any key closes the overlay without acting.
        assert_eq!(keys(&mut s, &ss, &[Key::Enter]), Step::Stay);
        assert_eq!((s.help, &s.screen, s.cursor), (false, &Screen::Action, 1));
        keys(&mut s, &ss, &[Key::Char('?')]);
        assert_eq!(keys(&mut s, &ss, &[Key::Interrupt]), Step::Interrupt);
        // In text forms `?` is typed.
        let mut s = State::new(StartScreen::Query, 1);
        assert!(s.text_entry());
        keys(&mut s, &ss, &[Key::Char('?')]);
        assert!(!s.help);
        assert!(matches!(&s.screen, Screen::Query(q) if q.input.text == "?"));
        let mut s = State::dialog(Screen::DirConfirm(DirInfo::default()));
        assert!(!s.text_entry());
        keys(&mut s, &[], &[Key::Char('?')]);
        assert!(s.help);
        assert_eq!(keys(&mut s, &[], &[Key::Esc]), Step::Stay);
        assert_eq!(s.screen, Screen::DirConfirm(DirInfo::default()));
    }

    #[test]
    fn dialogs() {
        let mut s = State::dialog(Screen::TrimConfirm(TrimInfo::default()));
        assert_eq!(
            keys(&mut s, &[], &[Key::Enter]),
            Step::Run(Action::TrimConfirm(TrimChoice::Resume))
        );
        assert_eq!(
            keys(&mut s, &[], &[Key::Down, Key::Enter]),
            Step::Run(Action::TrimConfirm(TrimChoice::Delete))
        );
        assert_eq!(
            keys(&mut s, &[], &[Key::Esc]),
            Step::Run(Action::TrimConfirm(TrimChoice::Cancel))
        );
        let mut s = State::dialog(Screen::TrimConfirm(TrimInfo {
            nothing_to_trim: true,
            ..TrimInfo::default()
        }));
        assert_eq!(
            keys(&mut s, &[], &[Key::Char('2'), Key::Enter]),
            Step::Run(Action::TrimConfirm(TrimChoice::Back))
        );
        let mut s = State::dialog(Screen::DirConfirm(DirInfo::default()));
        assert_eq!(
            keys(&mut s, &[], &[Key::Char('k'), Key::Enter]),
            Step::Run(Action::DirConfirm(DirChoice::No))
        );
        assert_eq!(
            keys(&mut s, &[], &[Key::Interrupt]),
            Step::Run(Action::DirConfirm(DirChoice::Cancel))
        );
        assert_eq!(
            keys(&mut s, &[], &[Key::Char('1'), Key::Enter]),
            Step::Run(Action::DirConfirm(DirChoice::Yes))
        );
    }
}
