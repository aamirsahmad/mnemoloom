//! TUI state and key handling (pure: no terminal, no index access).
//!
//! [`App::on_key`] mutates state and returns an [`Effect`] the event loop
//! carries out (re-filter, run an action, quit).
//!
//! Modal like yazi/helix: `Mode::Normal` moves within the focused pane
//! ([`Focus`]); `/` enters `Mode::Query` where keys edit the search.

use chrono::{Local, NaiveDate};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::facets::{self, Counts, Facet, ProjectSel};
use super::view::{Preview, ViewState};
use crate::index::dates::parse_date_now;
use crate::index::query::{Agents, Filters};
use crate::index::{resolve_dir, split_dir_branch};
use crate::session::Agent;

/// Session actions (the action menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    View,
    Path,
    Copy,
    CopyId,
    Export,
    Query,
    Resume,
    Clone,
    Trim,
    SmartTrim,
    Rollover,
    Delete,
}

impl Action {
    pub const ALL: [Action; 12] = [
        Action::View,
        Action::Path,
        Action::Copy,
        Action::CopyId,
        Action::Export,
        Action::Query,
        Action::Resume,
        Action::Clone,
        Action::Trim,
        Action::SmartTrim,
        Action::Rollover,
        Action::Delete,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::View => "(v) View full session",
            Action::Path => "(p) Show session file path",
            Action::Copy => "(c) Copy session file",
            Action::CopyId => "(i) Copy session ID to clipboard",
            Action::Export => "(e) Export to text file (.txt)",
            Action::Query => "(q) Query the session",
            Action::Resume => "(r) Resume as-is",
            Action::Clone => "(l) Clone session + resume clone",
            Action::Trim => "(t) Trim + resume...",
            Action::SmartTrim => "(s) Smart trim + resume...",
            Action::Rollover => "(o) Rollover: handoff work to fresh session...",
            Action::Delete => "(d) Delete session",
        }
    }

    pub fn shortcut(self) -> char {
        match self {
            Action::View => 'v',
            Action::Path => 'p',
            Action::Copy => 'c',
            Action::CopyId => 'i',
            Action::Export => 'e',
            Action::Query => 'q',
            Action::Resume => 'r',
            Action::Clone => 'l',
            Action::Trim => 't',
            Action::SmartTrim => 's',
            Action::Rollover => 'o',
            Action::Delete => 'd',
        }
    }
}

/// Filter-modal rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterItem {
    Reset,
    Original,
    Sub,
    Exec,
    Trimmed,
    Rollover,
    Live,
    AllAgents,
    ClaudeOnly,
    CodexOnly,
    PiOnly,
    OpenCodeOnly,
    MinLines,
    After,
    Before,
}

impl FilterItem {
    pub const ALL: [FilterItem; 15] = [
        FilterItem::Reset,
        FilterItem::Original,
        FilterItem::Sub,
        FilterItem::Exec,
        FilterItem::Trimmed,
        FilterItem::Rollover,
        FilterItem::Live,
        FilterItem::AllAgents,
        FilterItem::ClaudeOnly,
        FilterItem::CodexOnly,
        FilterItem::PiOnly,
        FilterItem::OpenCodeOnly,
        FilterItem::MinLines,
        FilterItem::After,
        FilterItem::Before,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FilterItem::Reset => "(x) Reset to defaults",
            FilterItem::Original => "(o) Include original sessions",
            FilterItem::Sub => "(s) Include sub-agent sessions",
            FilterItem::Exec => "(h) Include headless exec runs",
            FilterItem::Trimmed => "(t) Include trimmed sessions",
            FilterItem::Rollover => "(r) Include rollover sessions",
            FilterItem::Live => "(!) Live sessions only",
            FilterItem::AllAgents => "(a) All agents",
            FilterItem::ClaudeOnly => "(d) Claude only",
            FilterItem::CodexOnly => "(e) Codex only",
            FilterItem::PiOnly => "(i) pi only",
            FilterItem::OpenCodeOnly => "(n) opencode only",
            FilterItem::MinLines => "(l) Minimum lines",
            FilterItem::After => "(>) After date",
            FilterItem::Before => "(<) Before date",
        }
    }

    pub fn shortcut(self) -> char {
        self.label().chars().nth(1).unwrap_or(' ')
    }
}

/// Text-input prompts in the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    MinLines,
    Agent,
    JumpToRow,
    AfterDate,
    BeforeDate,
    ScopeDir,
}

/// The focused pane in `Mode::Normal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Filters,
    Results,
    Preview,
}

/// Terminal widths below which the filter / preview panes are hidden by
/// default (`f` / `p` override that).
pub const FILTERS_MIN_WIDTH: u16 = 110;
pub const PREVIEW_MIN_WIDTH: u16 = 70;

/// What has keyboard focus.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Pane navigation (see [`Focus`]).
    Normal,
    /// Typing the search query.
    Query,
    /// The key-help overlay.
    Help,
    /// `:` pressed; the next key is a command.
    Command,
    Input(InputKind, String),
    Scope(usize),
    Filter(usize),
    Actions(usize),
    ConfirmExit,
    ConfirmDelete,
    View(Box<ViewState>),
    /// The project picker: name filter and cursor over the matches.
    Projects {
        query: String,
        cursor: usize,
    },
}

/// Work for the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    None,
    /// Re-run the search now.
    Refilter,
    /// Re-run the search after the debounce delay.
    Debounce,
    /// Run an action on the selected session.
    Run(Action),
    Quit,
}

/// Search-screen state.
#[derive(Debug, Clone)]
pub struct App {
    pub query: String,
    pub filters: Filters,
    pub by_time: bool,
    pub limit: Option<usize>,
    /// Indices into the catalog rows, in display order.
    pub results: Vec<usize>,
    pub selected: usize,
    pub list_scroll: usize,
    pub mode: Mode,
    pub focus: Focus,
    pub status: Option<String>,
    /// Branch of the launch directory (scope modal label).
    pub current_branch: Option<String>,
    /// Project facet (TUI-only filter).
    pub projects: ProjectSel,
    /// Facet counts for the current query.
    pub counts: Counts,
    /// Cursor over [`App::facet_items`].
    pub facet_cursor: usize,
    /// Transcript of the selected session (may lag the selection while loading).
    pub preview: Option<Box<Preview>>,
    /// Pane visibility set by `f` / `p`; `None` follows the width default.
    pub show_filters: Option<bool>,
    pub show_preview: Option<bool>,
    /// Terminal width at the last draw.
    pub width: u16,
}

const PAGE: usize = 10;

fn ctrl(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL)
}

impl App {
    pub fn new(filters: Filters, query: String, by_time: bool, limit: Option<usize>) -> App {
        App {
            mode: if query.is_empty() {
                Mode::Query
            } else {
                Mode::Normal
            },
            query,
            filters,
            by_time,
            limit,
            results: Vec::new(),
            selected: 0,
            list_scroll: 0,
            focus: Focus::Results,
            status: None,
            current_branch: None,
            projects: ProjectSel::default(),
            counts: Counts::default(),
            facet_cursor: 0,
            preview: None,
            show_filters: None,
            show_preview: None,
            width: u16::MAX,
        }
    }

    /// Which of (filters, preview) are shown at `width`.
    pub fn panes(&self, width: u16) -> (bool, bool) {
        (
            self.show_filters.unwrap_or(width >= FILTERS_MIN_WIDTH),
            self.show_preview.unwrap_or(width >= PREVIEW_MIN_WIDTH),
        )
    }

    /// Record the terminal width; focus leaves a pane that got hidden.
    pub fn layout(&mut self, width: u16) {
        self.width = width;
        let (filters, preview) = self.panes(width);
        if (self.focus == Focus::Filters && !filters) || (self.focus == Focus::Preview && !preview)
        {
            self.focus = Focus::Results;
        }
    }

    /// Focusable panes, left to right.
    fn focus_order(&self) -> Vec<Focus> {
        let (filters, preview) = self.panes(self.width);
        let mut v = Vec::with_capacity(3);
        if filters {
            v.push(Focus::Filters);
        }
        v.push(Focus::Results);
        if preview {
            v.push(Focus::Preview);
        }
        v
    }

    fn move_focus(&mut self, delta: isize, wrap: bool) {
        let order = self.focus_order();
        let n = order.len() as isize;
        let i = order.iter().position(|f| *f == self.focus).unwrap_or(0) as isize + delta;
        let i = if wrap {
            i.rem_euclid(n)
        } else {
            i.clamp(0, n - 1)
        };
        self.focus = order[i as usize];
    }

    /// Replace the facet counts, keeping the filter cursor on the same
    /// facet (refilters reshape the project rows above it).
    pub fn set_counts(&mut self, counts: Counts) {
        let at = self.facet_at_cursor();
        self.counts = counts;
        if let Some(i) = at.and_then(|f| self.facet_items().iter().position(|g| *g == f)) {
            self.facet_cursor = i;
        }
    }

    /// Toggleable filter-pane rows.
    pub fn facet_items(&self) -> Vec<Facet> {
        facets::items(&self.counts, &self.projects)
    }

    fn facet_at_cursor(&self) -> Option<Facet> {
        let items = self.facet_items();
        items
            .get(self.facet_cursor.min(items.len().saturating_sub(1)))
            .cloned()
    }

    fn toggle_facet(&mut self, today: NaiveDate) -> Effect {
        let Some(item) = self.facet_at_cursor() else {
            return Effect::None;
        };
        if item == Facet::MoreProjects {
            return self.open_projects();
        }
        facets::toggle(&mut self.filters, &mut self.projects, &item, today);
        Effect::Refilter
    }

    /// `o` on a project row: show only that project (the cursor follows it
    /// to the top of the list).
    fn only_facet_project(&mut self) -> Effect {
        let Some(Facet::Project(p)) = self.facet_at_cursor() else {
            return Effect::None;
        };
        self.projects.only_this(&p);
        let at = Facet::Project(p);
        if let Some(i) = self.facet_items().iter().position(|f| *f == at) {
            self.facet_cursor = i;
        }
        Effect::Refilter
    }

    fn open_projects(&mut self) -> Effect {
        self.mode = Mode::Projects {
            query: String::new(),
            cursor: 0,
        };
        Effect::None
    }

    /// Picker rows matching `query`.
    pub fn picker_items(&self, query: &str) -> Vec<(String, usize)> {
        facets::picker_projects(&self.counts, &self.projects, query)
    }

    /// Whether quitting loses filter settings (asks first).
    fn filters_set(&self) -> bool {
        self.filters.has_active() || !self.projects.is_empty()
    }

    fn quit(&mut self) -> Effect {
        if self.filters_set() {
            self.mode = Mode::ConfirmExit;
            Effect::None
        } else {
            Effect::Quit
        }
    }

    fn reset_filters(&mut self, dates: bool) {
        self.filters.reset(dates);
        self.projects.clear();
    }

    /// Replace the results (after a search): selection back to the top.
    pub fn set_results(&mut self, results: Vec<usize>) {
        self.results = results;
        self.selected = 0;
        self.list_scroll = 0;
    }

    /// The selected row index, if any.
    pub fn selected_row(&self) -> Option<usize> {
        self.results.get(self.selected).copied()
    }

    /// Keep the selection inside a window of `visible` items.
    pub fn ensure_visible(&mut self, visible: usize) {
        if self.selected < self.list_scroll {
            self.list_scroll = self.selected;
        } else if visible > 0 && self.selected >= self.list_scroll + visible {
            self.list_scroll = self.selected + 1 - visible;
        }
    }

    fn move_by(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let max = self.results.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(max);
    }

    /// Handle one key press.
    pub fn on_key(&mut self, k: KeyEvent) -> Effect {
        self.status = None;
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.normal_key(k),
            Mode::Query => self.query_key(k),
            Mode::Help => Effect::None,
            Mode::Command => self.command_key(k),
            Mode::Input(kind, buf) => self.input_key(kind, buf, k),
            Mode::Scope(sel) => self.scope_key(sel, k),
            Mode::Filter(sel) => self.filter_key(sel, k),
            Mode::Actions(sel) => self.action_key(sel, k),
            Mode::Projects { query, cursor } => self.projects_key(query, cursor, k),
            Mode::ConfirmExit => match k.code {
                KeyCode::Enter | KeyCode::Char('y' | 'Y') => Effect::Quit,
                KeyCode::Esc | KeyCode::Char('n' | 'N') => Effect::None,
                _ => {
                    self.mode = Mode::ConfirmExit;
                    Effect::None
                }
            },
            Mode::ConfirmDelete => match k.code {
                KeyCode::Enter | KeyCode::Char('y' | 'Y') => Effect::Run(Action::Delete),
                KeyCode::Esc | KeyCode::Char('n' | 'N') => Effect::None,
                _ => {
                    self.mode = Mode::ConfirmDelete;
                    Effect::None
                }
            },
            Mode::View(mut v) => match v.on_key(k) {
                super::view::ViewEffect::Stay => {
                    self.mode = Mode::View(v);
                    Effect::None
                }
                super::view::ViewEffect::Close => Effect::None,
                super::view::ViewEffect::Quit => Effect::Quit,
            },
        }
    }

    fn normal_key(&mut self, k: KeyEvent) -> Effect {
        if ctrl(&k) {
            return match k.code {
                KeyCode::Char('c') => Effect::Quit,
                KeyCode::Char('f') => {
                    self.mode = Mode::Filter(0);
                    Effect::None
                }
                KeyCode::Char('g') => {
                    self.mode = Mode::Input(InputKind::JumpToRow, String::new());
                    Effect::None
                }
                KeyCode::Char('s') => {
                    self.by_time = !self.by_time;
                    Effect::Refilter
                }
                KeyCode::Char('u') => self.nav(-(PAGE as isize)),
                _ => Effect::None,
            };
        }
        let has = !self.results.is_empty();
        match k.code {
            KeyCode::Char(':') => self.mode = Mode::Command,
            KeyCode::Char('/') => self.mode = Mode::Query,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('s') => self.mode = Mode::Scope(0),
            KeyCode::Char('P') => return self.open_projects(),
            KeyCode::Char('q') => return self.quit(),
            // A typed query is cleared first; the next Esc quits.
            KeyCode::Esc if self.focus == Focus::Results && !self.query.is_empty() => {
                self.query.clear();
                return Effect::Refilter;
            }
            KeyCode::Esc if self.focus == Focus::Results => return self.quit(),
            KeyCode::Esc => self.focus = Focus::Results,
            KeyCode::Char('h') | KeyCode::Left => self.move_focus(-1, false),
            KeyCode::Char('l') | KeyCode::Right => self.move_focus(1, false),
            KeyCode::Tab => self.move_focus(1, true),
            KeyCode::BackTab => self.move_focus(-1, true),
            KeyCode::Char('f') => {
                self.show_filters = Some(!self.panes(self.width).0);
                self.layout(self.width);
            }
            KeyCode::Char('p') => {
                self.show_preview = Some(!self.panes(self.width).1);
                self.layout(self.width);
            }
            KeyCode::Char('n') => self.preview_do(|v| v.step(true)),
            KeyCode::Char('N') => self.preview_do(|v| v.step(false)),
            KeyCode::Char('a') if has => self.mode = Mode::Actions(0),
            KeyCode::Char('y') if has => return Effect::Run(Action::CopyId),
            KeyCode::Char('x') if has => return Effect::Run(Action::Export),
            KeyCode::Char('t') if has => return Effect::Run(Action::Trim),
            _ => {
                return match self.focus {
                    Focus::Filters => self.filters_key(k),
                    Focus::Results => self.results_key(k),
                    Focus::Preview => self.preview_key(k),
                };
            }
        }
        Effect::None
    }

    fn results_key(&mut self, k: KeyEvent) -> Effect {
        match k.code {
            KeyCode::Enter if !self.results.is_empty() => Effect::Run(Action::Resume),
            KeyCode::Up | KeyCode::Char('k') => self.nav(-1),
            KeyCode::Down | KeyCode::Char('j') => self.nav(1),
            KeyCode::PageUp => self.nav(-(PAGE as isize)),
            KeyCode::PageDown => self.nav(PAGE as isize),
            KeyCode::Home | KeyCode::Char('g') => {
                self.selected = 0;
                Effect::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.selected = self.results.len().saturating_sub(1);
                Effect::None
            }
            // Any other printable key types into the query.
            KeyCode::Char(c) if !ctrl(&k) && !c.is_whitespace() => {
                self.mode = Mode::Query;
                self.query.push(c);
                Effect::Debounce
            }
            _ => Effect::None,
        }
    }

    fn nav(&mut self, delta: isize) -> Effect {
        self.move_by(delta);
        Effect::None
    }

    fn filters_key(&mut self, k: KeyEvent) -> Effect {
        let last = self.facet_items().len().saturating_sub(1);
        let c = &mut self.facet_cursor;
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => *c = c.saturating_sub(1).min(last),
            KeyCode::Down | KeyCode::Char('j') => *c = (*c + 1).min(last),
            KeyCode::PageUp => *c = c.saturating_sub(PAGE),
            KeyCode::PageDown => *c = (*c + PAGE).min(last),
            KeyCode::Home | KeyCode::Char('g') => *c = 0,
            KeyCode::End | KeyCode::Char('G') => *c = last,
            KeyCode::Char(' ') | KeyCode::Enter => {
                return self.toggle_facet(Local::now().date_naive());
            }
            KeyCode::Char('o') => return self.only_facet_project(),
            _ => {}
        }
        Effect::None
    }

    fn preview_key(&mut self, k: KeyEvent) -> Effect {
        match k.code {
            KeyCode::Enter if !self.results.is_empty() => return Effect::Run(Action::Resume),
            KeyCode::Up | KeyCode::Char('k') => self.preview_do(|v| v.scroll_by(-1)),
            KeyCode::Down | KeyCode::Char('j') => self.preview_do(|v| v.scroll_by(1)),
            KeyCode::PageUp => self.preview_do(|v| v.scroll_by(-v.page())),
            KeyCode::PageDown => self.preview_do(|v| v.scroll_by(v.page())),
            KeyCode::Home | KeyCode::Char('g') => self.preview_do(ViewState::top),
            KeyCode::End | KeyCode::Char('G') => self.preview_do(ViewState::bottom),
            _ => {}
        }
        Effect::None
    }

    /// Apply `f` to the preview of the selected session, if it is loaded.
    fn preview_do(&mut self, f: impl FnOnce(&mut ViewState)) {
        if let Some(p) = self.preview.as_mut() {
            p.fresh = false;
            f(&mut p.view);
        }
    }

    fn query_key(&mut self, k: KeyEvent) -> Effect {
        // Ctrl shortcuts (^s, ^f, ^g, …) work while typing too.
        if ctrl(&k) && !matches!(k.code, KeyCode::Char('c' | 'u')) {
            let effect = self.normal_key(k);
            if self.mode == Mode::Normal {
                self.mode = Mode::Query;
            }
            return effect;
        }
        self.mode = Mode::Query;
        match k.code {
            KeyCode::Char('c') if ctrl(&k) => return Effect::Quit,
            KeyCode::Char('u') if ctrl(&k) => {
                self.query.clear();
                return Effect::Debounce;
            }
            KeyCode::Char(c) if !ctrl(&k) => {
                self.query.push(c);
                return Effect::Debounce;
            }
            KeyCode::Backspace => {
                self.query.pop();
                return Effect::Debounce;
            }
            KeyCode::Esc | KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.focus = Focus::Results;
            }
            // Leaving the query by Tab lands on the filters: typing `h`
            // there would only edit the query.
            KeyCode::Tab | KeyCode::BackTab => {
                self.mode = Mode::Normal;
                self.focus = self.focus_order()[0];
            }
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::PageUp => self.move_by(-(PAGE as isize)),
            KeyCode::PageDown => self.move_by(PAGE as isize),
            _ => {}
        }
        Effect::None
    }

    fn command_key(&mut self, k: KeyEvent) -> Effect {
        let f = &mut self.filters;
        match k.code {
            KeyCode::Char('x' | '0') => {
                self.reset_filters(true);
                return Effect::Refilter;
            }
            KeyCode::Char('o') => f.include_original = !f.include_original,
            KeyCode::Char('s') => f.include_sub = !f.include_sub,
            KeyCode::Char('h') => f.include_exec = !f.include_exec,
            KeyCode::Char('t') => f.include_trimmed = !f.include_trimmed,
            KeyCode::Char('c') => f.include_continued = !f.include_continued,
            KeyCode::Char('!') => f.live_only = !f.live_only,
            KeyCode::Char('a' | 'd') => return self.open_input(InputKind::Agent),
            KeyCode::Char('m') => return self.open_input(InputKind::MinLines),
            KeyCode::Char('>') => return self.open_input(InputKind::AfterDate),
            KeyCode::Char('<') => return self.open_input(InputKind::BeforeDate),
            _ => return Effect::None,
        }
        Effect::Refilter
    }

    fn open_input(&mut self, kind: InputKind) -> Effect {
        let buf = if kind == InputKind::ScopeDir {
            self.scope_prefill()
        } else {
            String::new()
        };
        self.mode = Mode::Input(kind, buf);
        Effect::None
    }

    /// `<dir or launch cwd>[:<branch>]`.
    pub fn scope_prefill(&self) -> String {
        let dir = self
            .filters
            .dir
            .as_deref()
            .unwrap_or(&self.filters.launch_cwd);
        match &self.filters.branch {
            Some(b) => format!("{dir}:{b}"),
            None => dir.to_string(),
        }
    }

    fn input_key(&mut self, kind: InputKind, mut buf: String, k: KeyEvent) -> Effect {
        if kind == InputKind::Agent {
            let agents = match k.code {
                KeyCode::Char('0') => Agents::ALL,
                KeyCode::Char(c @ '1'..='4') => {
                    Agents::only(Agent::ALL[usize::from(c as u8 - b'1')])
                }
                KeyCode::Esc => return Effect::None,
                _ => {
                    self.mode = Mode::Input(kind, buf);
                    return Effect::None;
                }
            };
            self.filters.agents = agents;
            return Effect::Refilter;
        }
        let digits_only = matches!(kind, InputKind::MinLines | InputKind::JumpToRow);
        match k.code {
            KeyCode::Esc => return Effect::None,
            KeyCode::Enter => return self.apply_input(kind, buf.trim()),
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) if !ctrl(&k) && (!digits_only || c.is_ascii_digit()) => buf.push(c),
            _ => {}
        }
        self.mode = Mode::Input(kind, buf);
        Effect::None
    }

    fn apply_input(&mut self, kind: InputKind, buf: &str) -> Effect {
        match kind {
            InputKind::MinLines => {
                if let Ok(n) = buf.parse::<i64>() {
                    self.filters.min_lines = (n > 0).then_some(n);
                }
                Effect::Refilter
            }
            InputKind::JumpToRow => {
                if let Ok(n) = buf.parse::<usize>()
                    && (1..=self.results.len()).contains(&n)
                {
                    self.selected = n - 1;
                }
                Effect::None
            }
            InputKind::AfterDate | InputKind::BeforeDate => {
                let parsed = if buf.is_empty() {
                    Some(None)
                } else {
                    parse_date_now(buf).map(Some)
                };
                match parsed {
                    Some(d) if kind == InputKind::AfterDate => self.filters.after = d,
                    Some(d) => self.filters.before = d,
                    None => self.status = Some(format!("Unrecognized date: {buf}")),
                }
                Effect::Refilter
            }
            InputKind::ScopeDir => {
                self.apply_scope(buf);
                Effect::Refilter
            }
            InputKind::Agent => Effect::None,
        }
    }

    /// `dir:branch` | `:branch` | `dir` | empty (= global).
    fn apply_scope(&mut self, spec: &str) {
        let (dir, branch) = split_dir_branch(spec);
        let f = &mut self.filters;
        if !dir.is_empty() {
            f.dir = Some(resolve_dir(dir, &f.launch_cwd));
            f.global = false;
        } else if !spec.contains(':') {
            f.global = true;
            f.dir = None;
        }
        f.branch = branch.map(str::to_string);
    }

    fn scope_key(&mut self, sel: usize, k: KeyEvent) -> Effect {
        let choose = |app: &mut App, i: usize| -> Effect {
            match i {
                0 => {
                    app.filters.global = true;
                    app.filters.dir = None;
                    Effect::Refilter
                }
                1 => {
                    app.filters.global = false;
                    app.filters.dir = None;
                    Effect::Refilter
                }
                _ => app.open_input(InputKind::ScopeDir),
            }
        };
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.mode = Mode::Scope(sel.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.mode = Mode::Scope((sel + 1).min(2)),
            KeyCode::Enter | KeyCode::Char(' ') => return choose(self, sel),
            KeyCode::Char(c @ '1'..='3') => return choose(self, c as usize - '1' as usize),
            KeyCode::Esc | KeyCode::Char('/') => {}
            _ => self.mode = Mode::Scope(sel),
        }
        Effect::None
    }

    fn filter_key(&mut self, sel: usize, k: KeyEvent) -> Effect {
        let n = FilterItem::ALL.len();
        let item = match k.code {
            KeyCode::Esc => return Effect::None,
            KeyCode::Char('f') if ctrl(&k) => return Effect::None,
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Filter(sel.saturating_sub(1));
                return Effect::None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.mode = Mode::Filter((sel + 1).min(n - 1));
                return Effect::None;
            }
            KeyCode::Enter | KeyCode::Char(' ') => FilterItem::ALL[sel],
            KeyCode::Char(c) => match FilterItem::ALL.iter().position(|i| i.shortcut() == c) {
                Some(i) => FilterItem::ALL[i],
                None => {
                    self.mode = Mode::Filter(sel);
                    return Effect::None;
                }
            },
            _ => {
                self.mode = Mode::Filter(sel);
                return Effect::None;
            }
        };
        let pos = FilterItem::ALL
            .iter()
            .position(|i| *i == item)
            .unwrap_or(sel);
        if item == FilterItem::Reset {
            self.projects.clear();
        }
        let f = &mut self.filters;
        match item {
            FilterItem::Reset => f.reset(false),
            FilterItem::Original => f.include_original = !f.include_original,
            FilterItem::Sub => f.include_sub = !f.include_sub,
            FilterItem::Exec => f.include_exec = !f.include_exec,
            FilterItem::Trimmed => f.include_trimmed = !f.include_trimmed,
            FilterItem::Rollover => f.include_continued = !f.include_continued,
            FilterItem::Live => f.live_only = !f.live_only,
            FilterItem::AllAgents => f.agents = Agents::ALL,
            FilterItem::ClaudeOnly => f.agents = Agents::only(Agent::Claude),
            FilterItem::CodexOnly => f.agents = Agents::only(Agent::Codex),
            FilterItem::PiOnly => f.agents = Agents::only(Agent::Pi),
            FilterItem::OpenCodeOnly => f.agents = Agents::only(Agent::OpenCode),
            FilterItem::MinLines => return self.open_input(InputKind::MinLines),
            FilterItem::After => return self.open_input(InputKind::AfterDate),
            FilterItem::Before => return self.open_input(InputKind::BeforeDate),
        }
        self.mode = Mode::Filter(pos);
        Effect::Refilter
    }

    fn projects_key(&mut self, mut query: String, cursor: usize, k: KeyEvent) -> Effect {
        let items = self.picker_items(&query);
        let last = items.len().saturating_sub(1);
        let mut cursor = cursor.min(last);
        let effect = match k.code {
            KeyCode::Esc => return Effect::None,
            KeyCode::Char('a') if ctrl(&k) => {
                self.projects.clear();
                Effect::Refilter
            }
            KeyCode::Enter => match items.get(cursor) {
                Some((p, _)) => {
                    self.projects.only_this(p);
                    return Effect::Refilter;
                }
                None => Effect::None,
            },
            KeyCode::Char(' ') => match items.get(cursor) {
                Some((p, _)) => {
                    self.projects.toggle(p);
                    Effect::Refilter
                }
                None => Effect::None,
            },
            KeyCode::Char(c) if !ctrl(&k) => {
                query.push(c);
                cursor = 0;
                Effect::None
            }
            KeyCode::Backspace => {
                query.pop();
                cursor = 0;
                Effect::None
            }
            KeyCode::Up => {
                cursor = cursor.saturating_sub(1);
                Effect::None
            }
            KeyCode::Down => {
                cursor = (cursor + 1).min(last);
                Effect::None
            }
            KeyCode::PageUp => {
                cursor = cursor.saturating_sub(PAGE);
                Effect::None
            }
            KeyCode::PageDown => {
                cursor = (cursor + PAGE).min(last);
                Effect::None
            }
            KeyCode::Home => {
                cursor = 0;
                Effect::None
            }
            KeyCode::End => {
                cursor = last;
                Effect::None
            }
            _ => Effect::None,
        };
        self.mode = Mode::Projects { query, cursor };
        effect
    }

    fn action_key(&mut self, sel: usize, k: KeyEvent) -> Effect {
        let n = Action::ALL.len();
        let action = match k.code {
            KeyCode::Esc => return Effect::None,
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Actions(sel.saturating_sub(1));
                return Effect::None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.mode = Mode::Actions((sel + 1).min(n - 1));
                return Effect::None;
            }
            KeyCode::Enter | KeyCode::Char(' ') => Action::ALL[sel],
            KeyCode::Char(c) => match Action::ALL.iter().find(|a| a.shortcut() == c) {
                Some(a) => *a,
                None => {
                    self.mode = Mode::Actions(sel);
                    return Effect::None;
                }
            },
            _ => {
                self.mode = Mode::Actions(sel);
                return Effect::None;
            }
        };
        if action == Action::Delete {
            self.mode = Mode::ConfirmDelete;
            return Effect::None;
        }
        Effect::Run(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Homes;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn ctl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// RESULTS focus at 140 columns (all panes shown), 30 results.
    fn app() -> App {
        let f = Filters::new(&Homes::new("/h/c", "/h/x"), "/w/proj".into());
        let mut a = App::new(f, String::new(), false, None);
        a.mode = Mode::Normal;
        a.layout(140);
        a.set_results((0..30).collect());
        a
    }

    #[test]
    fn typing_navigation_and_quit() {
        let mut a = app();
        a.on_key(ch('/'));
        assert_eq!(a.on_key(ch('j')), Effect::Debounce);
        assert_eq!(a.on_key(ch('q')), Effect::Debounce);
        assert_eq!(a.query, "jq");
        assert_eq!(a.on_key(key(KeyCode::Backspace)), Effect::Debounce);
        assert_eq!(a.on_key(ctl('u')), Effect::Debounce);
        assert_eq!(a.query, "");
        assert_eq!(a.on_key(key(KeyCode::Esc)), Effect::None);
        assert_eq!(a.mode, Mode::Normal);
        a.on_key(key(KeyCode::PageDown));
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.selected, 11);
        a.on_key(ctl('u'));
        assert_eq!(a.selected, 1);
        a.on_key(key(KeyCode::End));
        assert_eq!(a.selected, 29);
        a.on_key(key(KeyCode::PageDown));
        assert_eq!(a.selected, 29);
        a.on_key(ch('g'));
        a.on_key(ch('j'));
        a.on_key(ch('j'));
        a.on_key(ch('k'));
        assert_eq!(a.selected, 1);
        a.on_key(ch('G'));
        assert_eq!(a.selected, 29);
        a.ensure_visible(5);
        assert_eq!(a.list_scroll, 25);
        assert_eq!(a.on_key(ctl('s')), Effect::Refilter);
        assert!(a.by_time);
        assert_eq!(a.on_key(key(KeyCode::Esc)), Effect::Quit);
        assert_eq!(a.on_key(ctl('c')), Effect::Quit);
    }

    #[test]
    fn esc_with_filters_confirms() {
        let mut a = app();
        a.on_key(ch(':'));
        assert_eq!(a.on_key(ch('s')), Effect::Refilter);
        assert!(a.filters.include_sub);
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.on_key(key(KeyCode::Esc)), Effect::None);
        assert_eq!(a.mode, Mode::ConfirmExit);
        assert_eq!(a.on_key(ch('n')), Effect::None);
        assert_eq!(a.mode, Mode::Normal);
        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::Quit);
    }

    #[test]
    fn command_mode_agent_both_keys() {
        for k in ['a', 'd'] {
            let mut a = app();
            a.on_key(ch(':'));
            a.on_key(ch(k));
            assert_eq!(a.mode, Mode::Input(InputKind::Agent, String::new()));
            a.on_key(ch('x'));
            assert!(matches!(a.mode, Mode::Input(InputKind::Agent, _)));
            assert_eq!(a.on_key(ch('2')), Effect::Refilter);
            assert_eq!(a.filters.agents, Agents::only(Agent::Codex));
        }
        for (k, agent) in ['1', '2', '3', '4'].into_iter().zip(Agent::ALL) {
            let mut a = app();
            a.on_key(ch(':'));
            a.on_key(ch('a'));
            a.on_key(ch(k));
            assert_eq!(a.filters.agents.single(), Some(agent));
            a.on_key(ch(':'));
            a.on_key(ch('a'));
            a.on_key(ch('0'));
            assert!(a.filters.agents.is_all());
        }
        let mut a = app();
        a.filters.agents = Agents::only(Agent::Claude);
        a.filters.after = parse_date_now("2026-01-01");
        a.on_key(ch(':'));
        a.on_key(ch('x'));
        assert_eq!((a.filters.agents, a.filters.after), (Agents::ALL, None));
    }

    #[test]
    fn input_modes() {
        let mut a = app();
        a.on_key(ch(':'));
        a.on_key(ch('m'));
        for c in ['1', 'z', '5'] {
            a.on_key(ch(c));
        }
        assert_eq!(a.mode, Mode::Input(InputKind::MinLines, "15".into()));
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::Refilter);
        assert_eq!(a.filters.min_lines, Some(15));

        a.on_key(ctl('g'));
        a.on_key(ch('7'));
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.selected, 6);

        a.on_key(ch(':'));
        a.on_key(ch('>'));
        for c in "2026-10-01T16:45".chars() {
            a.on_key(ch(c));
        }
        a.on_key(key(KeyCode::Enter));
        assert_eq!(
            a.filters.after.map(|d| d.display()).as_deref(),
            Some("10/01/26 16:45")
        );
        a.on_key(ch(':'));
        a.on_key(ch('>'));
        a.on_key(ch('?'));
        a.on_key(key(KeyCode::Enter));
        assert!(a.filters.after.is_some());
        assert_eq!(a.status.as_deref(), Some("Unrecognized date: ?"));
        a.on_key(ch(':'));
        a.on_key(ch('>'));
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.filters.after, None);
    }

    #[test]
    fn scope_modal_and_input() {
        let mut a = app();
        a.on_key(ch('s'));
        assert_eq!(a.mode, Mode::Scope(0));
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::Refilter);
        assert!(a.filters.global);
        a.on_key(ch('s'));
        assert_eq!(a.on_key(ch('2')), Effect::Refilter);
        assert!(!a.filters.global);
        a.on_key(ch('s'));
        a.on_key(ch('3'));
        assert_eq!(a.mode, Mode::Input(InputKind::ScopeDir, "/w/proj".into()));
        for c in ":dev".chars() {
            a.on_key(ch(c));
        }
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.filters.branch.as_deref(), Some("dev"));
        assert_eq!(a.filters.dir.as_deref(), Some("/w/proj"));
        a.apply_scope(":feature");
        assert_eq!(a.filters.branch.as_deref(), Some("feature"));
        a.apply_scope("");
        assert!(a.filters.global && a.filters.dir.is_none() && a.filters.branch.is_none());
        a.apply_scope("/tmp/nonexistent-x:feat/y");
        assert_eq!(a.filters.dir.as_deref(), Some("/tmp/nonexistent-x"));
        assert_eq!(a.filters.branch.as_deref(), Some("feat/y"));
    }

    #[test]
    fn filter_modal() {
        let mut a = app();
        a.on_key(ctl('f'));
        assert_eq!(a.on_key(ch('h')), Effect::Refilter);
        assert!(a.filters.include_exec);
        assert_eq!(a.mode, Mode::Filter(3));
        a.on_key(ch('!'));
        assert!(a.filters.live_only);
        a.on_key(ch('e'));
        assert_eq!(a.filters.agents, Agents::only(Agent::Codex));
        a.on_key(ch('i'));
        assert_eq!(a.filters.agents, Agents::only(Agent::Pi));
        a.on_key(ch('n'));
        assert_eq!(a.filters.agents, Agents::only(Agent::OpenCode));
        a.filters.after = parse_date_now("2026-01-01");
        a.on_key(ch('x'));
        assert!(!a.filters.include_exec && !a.filters.live_only && a.filters.agents.is_all());
        assert!(a.filters.after.is_some());
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.mode, Mode::Filter(1));
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::Refilter);
        assert!(!a.filters.include_original);
        a.on_key(ch('l'));
        assert_eq!(a.mode, Mode::Input(InputKind::MinLines, String::new()));
        a.on_key(key(KeyCode::Esc));
        a.on_key(ctl('f'));
        a.on_key(ctl('f'));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn action_menu() {
        let mut a = app();
        a.on_key(ch('a'));
        assert_eq!(a.mode, Mode::Actions(0));
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::Run(Action::Path));
        a.on_key(ch('a'));
        assert_eq!(a.on_key(ch('r')), Effect::Run(Action::Resume));
        a.on_key(ch('a'));
        assert_eq!(a.on_key(ch('d')), Effect::None);
        assert_eq!(a.mode, Mode::ConfirmDelete);
        assert_eq!(a.on_key(ch('y')), Effect::Run(Action::Delete));
        a.on_key(ch('a'));
        a.on_key(ch('d'));
        assert_eq!(a.on_key(key(KeyCode::Esc)), Effect::None);
        assert_eq!(a.mode, Mode::Normal);
        a.set_results(Vec::new());
        a.on_key(ch('a'));
        assert_eq!(a.mode, Mode::Query, "no session: `a` types instead");
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::None);
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::None);
    }

    #[test]
    fn direct_session_keys() {
        let mut a = app();
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::Run(Action::Resume));
        assert_eq!(a.on_key(ch('y')), Effect::Run(Action::CopyId));
        assert_eq!(a.on_key(ch('x')), Effect::Run(Action::Export));
        assert_eq!(a.on_key(ch('t')), Effect::Run(Action::Trim));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn query_mode_transitions() {
        let f = Filters::new(&Homes::new("/h/c", "/h/x"), "/w".into());
        // No query at launch: start typing; with one: start in RESULTS.
        assert_eq!(
            App::new(f.clone(), String::new(), false, None).mode,
            Mode::Query
        );
        let mut a = App::new(f, "auth".into(), false, None);
        assert_eq!((a.mode.clone(), a.focus), (Mode::Normal, Focus::Results));
        a.layout(140);
        a.set_results((0..5).collect());
        a.on_key(ch('l'));
        assert_eq!(a.focus, Focus::Preview);
        // `/` from any pane edits the query; Enter / Esc land on RESULTS.
        a.on_key(ch('/'));
        assert_eq!(a.mode, Mode::Query);
        for c in " q?h".chars() {
            assert_eq!(a.on_key(ch(c)), Effect::Debounce);
        }
        assert_eq!(a.query, "auth q?h");
        assert_eq!(a.mode, Mode::Query);
        a.on_key(key(KeyCode::Down));
        a.on_key(key(KeyCode::Down));
        assert_eq!((a.selected, a.query.as_str()), (2, "auth q?h"));
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::None);
        assert_eq!((a.mode.clone(), a.focus), (Mode::Normal, Focus::Results));
        // Tab from the query goes straight to the filters.
        a.on_key(ch('/'));
        a.on_key(key(KeyCode::Tab));
        assert_eq!((a.mode.clone(), a.focus), (Mode::Normal, Focus::Filters));
        a.focus = Focus::Results;
        a.on_key(ch('/'));
        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.on_key(ctl('c')), Effect::Quit);
    }

    #[test]
    fn pane_focus_and_narrow_widths() {
        let mut a = app();
        a.on_key(ch('h'));
        assert_eq!(a.focus, Focus::Filters);
        a.on_key(ch('h'));
        assert_eq!(a.focus, Focus::Filters, "h stops at the left edge");
        a.on_key(ch('l'));
        a.on_key(ch('l'));
        assert_eq!(a.focus, Focus::Preview);
        a.on_key(key(KeyCode::Tab));
        assert_eq!(a.focus, Focus::Filters, "tab wraps");
        a.on_key(key(KeyCode::BackTab));
        assert_eq!(a.focus, Focus::Preview);
        // Esc from another pane returns to RESULTS instead of quitting.
        assert_eq!(a.on_key(key(KeyCode::Esc)), Effect::None);
        assert_eq!(a.focus, Focus::Results);
        // Below 110 columns the filter pane is gone; `f` brings it back.
        a.on_key(ch('h'));
        a.layout(80);
        assert_eq!(a.focus, Focus::Results, "focus leaves a hidden pane");
        assert_eq!(a.panes(80), (false, true));
        a.on_key(ch('h'));
        assert_eq!(a.focus, Focus::Results);
        a.on_key(ch('f'));
        assert_eq!(a.panes(80), (true, true));
        a.on_key(ch('h'));
        assert_eq!(a.focus, Focus::Filters);
        // Below 70 the preview goes too; `p` flips it.
        a.on_key(ch('f'));
        assert_eq!(a.focus, Focus::Results);
        assert_eq!(a.panes(50), (false, false));
        a.layout(50);
        a.on_key(ch('l'));
        assert_eq!(a.focus, Focus::Results);
        a.on_key(ch('p'));
        assert_eq!(a.panes(50), (false, true));
        a.on_key(ch('l'));
        assert_eq!(a.focus, Focus::Preview);
    }

    #[test]
    fn help_and_quit_keys() {
        let mut a = app();
        a.on_key(ch('?'));
        assert_eq!(a.mode, Mode::Help);
        assert_eq!(a.on_key(ch('q')), Effect::None, "any key closes help");
        assert_eq!(a.mode, Mode::Normal);
        a.on_key(ch('l'));
        assert_eq!(a.on_key(ch('q')), Effect::Quit, "q quits from any pane");
        // Hidden projects count as filters worth confirming.
        let mut a = app();
        a.projects.hidden.insert("webapp".into());
        assert_eq!(a.on_key(ch('q')), Effect::None);
        assert_eq!(a.mode, Mode::ConfirmExit);
        a.on_key(key(KeyCode::Esc));
        a.on_key(ch(':'));
        assert_eq!(a.on_key(ch('x')), Effect::Refilter);
        assert!(a.projects.is_empty());
        assert_eq!(a.on_key(key(KeyCode::Esc)), Effect::Quit);
    }

    #[test]
    fn facet_cursor_toggles_filters() {
        let mut a = app();
        a.counts = Counts {
            claude: 3,
            codex: 2,
            pi: 1,
            opencode: 0,
            projects: vec![("webapp".into(), 4), ("infra".into(), 1)],
        };
        a.on_key(ch('h'));
        assert_eq!(a.focus, Focus::Filters);
        // Items: claude, codex, pi, opencode, webapp, infra, today, yesterday,
        // 3d, 7d, 30d, all, originals, derived, sidechains.
        assert_eq!(a.facet_items().len(), 15);
        assert_eq!(a.on_key(ch(' ')), Effect::Refilter);
        assert!(!a.filters.agents.contains(Agent::Claude));
        assert!(a.filters.agents.contains(Agent::Codex) && a.filters.agents.contains(Agent::Pi));
        for _ in 0..4 {
            a.on_key(ch('j'));
        }
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::Refilter);
        assert!(!a.projects.shows("webapp"));
        for _ in 0..5 {
            a.on_key(ch('j'));
        }
        a.on_key(ch(' '));
        assert_eq!(
            a.filters.after,
            facets::When::Week.bound(Local::now().date_naive())
        );
        a.on_key(ch('G'));
        assert_eq!(a.facet_cursor, 14);
        a.on_key(ch('j'));
        assert_eq!(a.facet_cursor, 14, "cursor clamps");
        a.on_key(ch(' '));
        assert!(a.filters.include_sub);
        a.on_key(ch('g'));
        assert_eq!(a.facet_cursor, 0);
        // j/k in the filter pane don't move the results selection.
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn facet_cursor_survives_refilters() {
        let mut a = app();
        let counts = |n: usize| Counts {
            claude: 3,
            codex: 2,
            pi: 1,
            opencode: 0,
            projects: (0..n).map(|i| (format!("p{i}"), 1)).collect(),
        };
        a.set_counts(counts(5));
        let week = Facet::When(facets::When::Week);
        a.facet_cursor = a.facet_items().iter().position(|f| *f == week).unwrap();
        // A narrower filter drops project rows above the cursor.
        a.set_counts(counts(1));
        assert_eq!(a.facet_items()[a.facet_cursor], week);
    }

    #[test]
    fn ctrl_keys_work_while_typing() {
        let mut a = app();
        a.on_key(ch('/'));
        a.on_key(ch('x'));
        assert_eq!(a.on_key(ctl('s')), Effect::Refilter);
        assert!(a.by_time);
        assert_eq!(a.mode, Mode::Query, "still typing");
        a.on_key(ctl('g'));
        assert_eq!(a.mode, Mode::Input(InputKind::JumpToRow, String::new()));
        a.on_key(ch('3'));
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.selected, 2);
        a.on_key(ch('/'));
        a.on_key(ctl('f'));
        assert_eq!(a.mode, Mode::Filter(0));
        a.on_key(key(KeyCode::Esc));
        a.on_key(ch('/'));
        assert_eq!(a.on_key(ctl('u')), Effect::Debounce, "^u still clears");
        assert_eq!((a.query.as_str(), &a.mode), ("", &Mode::Query));
        assert_eq!(a.on_key(ctl('c')), Effect::Quit);
    }

    #[test]
    fn typing_in_results_edits_the_query() {
        let mut a = app();
        a.query = "auth".into();
        assert_eq!(a.on_key(ch('z')), Effect::Debounce);
        assert_eq!((a.query.as_str(), &a.mode), ("authz", &Mode::Query));
        a.on_key(key(KeyCode::Enter));
        // Bound keys keep their meaning; space doesn't start a query.
        assert_eq!(a.on_key(ch('y')), Effect::Run(Action::CopyId));
        assert_eq!(a.on_key(ch('j')), Effect::None);
        assert_eq!(a.on_key(ch(' ')), Effect::None);
        assert_eq!((a.query.as_str(), &a.mode), ("authz", &Mode::Normal));
        // Esc clears the query first, then quits.
        assert_eq!(a.on_key(key(KeyCode::Esc)), Effect::Refilter);
        assert_eq!((a.query.as_str(), &a.mode), ("", &Mode::Normal));
        assert_eq!(a.on_key(key(KeyCode::Esc)), Effect::Quit);
        // Other panes don't type.
        let mut a = app();
        a.on_key(ch('l'));
        assert_eq!(a.on_key(ch('z')), Effect::None);
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn hidden_panes_stay_hidden_across_resizes() {
        let mut a = app();
        a.on_key(ch('f'));
        assert!(!a.panes(140).0);
        a.layout(100);
        assert!(!a.panes(100).0, "narrowing doesn't bring it back");
        a.layout(140);
        a.on_key(ch('f'));
        assert!(a.panes(140).0);
        assert!(a.panes(60).0, "shown on request stays shown");
        a.on_key(ch('p'));
        assert!(!a.panes(60).1);
        assert!(!a.panes(200).1);
    }

    /// 12 projects: p00 (20) .. p11 (9); the pane lists 8 plus "…4 more".
    fn many_projects() -> App {
        let mut a = app();
        a.counts = Counts {
            claude: 3,
            codex: 2,
            pi: 1,
            opencode: 0,
            projects: (0..12).map(|i| (format!("p{i:02}"), 20 - i)).collect(),
        };
        a
    }

    fn picker(a: &App) -> (String, usize) {
        match &a.mode {
            Mode::Projects { query, cursor } => (query.clone(), *cursor),
            m => panic!("not in the picker: {m:?}"),
        }
    }

    #[test]
    fn more_projects_row_opens_the_picker() {
        let mut a = many_projects();
        a.on_key(ch('h'));
        let more = 4 + facets::TOP_PROJECTS;
        assert_eq!(a.facet_items()[more], Facet::MoreProjects);
        for _ in 0..more {
            a.on_key(ch('j'));
        }
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::None);
        assert_eq!(picker(&a), (String::new(), 0));
        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Normal);
        a.on_key(ch(' '));
        assert!(
            matches!(a.mode, Mode::Projects { .. }),
            "space opens it too"
        );
        assert!(a.projects.is_empty());
        // `P` opens it from any pane.
        a.on_key(key(KeyCode::Esc));
        a.on_key(ch('l'));
        a.on_key(ch('P'));
        assert!(matches!(a.mode, Mode::Projects { .. }));
    }

    #[test]
    fn picker_keys() {
        let mut a = many_projects();
        a.on_key(ch('P'));
        a.on_key(key(KeyCode::End));
        assert_eq!(picker(&a).1, 11);
        a.on_key(key(KeyCode::PageUp));
        a.on_key(key(KeyCode::Up));
        assert_eq!(picker(&a).1, 0);
        // Typing filters (and resets the cursor); space stays a toggle.
        for c in "P1".chars() {
            assert_eq!(a.on_key(ch(c)), Effect::None);
        }
        assert_eq!(picker(&a), ("P1".into(), 0));
        let names: Vec<String> = a.picker_items("P1").into_iter().map(|p| p.0).collect();
        assert_eq!(names, ["p10", "p11"]);
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.on_key(ch(' ')), Effect::Refilter);
        assert!(!a.projects.shows("p11") && a.projects.shows("p10"));
        a.on_key(key(KeyCode::Backspace));
        assert_eq!(picker(&a), ("P".into(), 0));
        a.on_key(key(KeyCode::Down));
        a.on_key(key(KeyCode::Down));
        // Enter: only this project, and close.
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::Refilter);
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.projects.only, ["p02".to_string()].into());
        assert!(a.projects.hidden.is_empty());
        // With an only set, space adds to it; ^a clears everything.
        a.on_key(ch('P'));
        assert_eq!(a.on_key(ch(' ')), Effect::Refilter);
        assert!(a.projects.shows("p00") && a.projects.shows("p02"));
        assert_eq!(a.on_key(ctl('a')), Effect::Refilter);
        assert!(a.projects.is_empty());
        assert!(matches!(a.mode, Mode::Projects { .. }));
        // Nothing matches: keys don't panic, Enter does nothing (stays open).
        a.on_key(ch('z'));
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.on_key(ch(' ')), Effect::None);
        assert_eq!(a.on_key(key(KeyCode::Enter)), Effect::None);
        assert_eq!(picker(&a), ("z".into(), 0));
        assert_eq!(a.on_key(key(KeyCode::Esc)), Effect::None);
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn o_shows_only_the_project_under_the_cursor() {
        let mut a = many_projects();
        a.on_key(ch('h'));
        assert_eq!(a.on_key(ch('o')), Effect::None, "not on a project row");
        for _ in 0..7 {
            a.on_key(ch('j'));
        }
        assert_eq!(a.facet_items()[7], Facet::Project("p03".into()));
        assert_eq!(a.on_key(ch('o')), Effect::Refilter);
        assert_eq!(a.projects.only, ["p03".to_string()].into());
        assert_eq!(a.facet_cursor, 4, "cursor follows it to the top");
        assert!(a.filters_set());
        a.on_key(ch('j'));
        a.on_key(ch(' '));
        assert!(a.projects.shows("p00") && a.projects.shows("p03"));
    }

    #[test]
    fn preview_keys_scroll_and_jump_matches() {
        let mut a = app();
        let lines: Vec<String> = (0..60)
            .map(|i| format!("> line {i}{}", if i % 20 == 5 { " auth" } else { "" }))
            .collect();
        let mut v = ViewState::new(
            facets::tests::row(0, Agent::Claude, "p"),
            lines,
            vec!["auth".into()],
        );
        v.layout(60, 10);
        a.preview = Some(Box::new(Preview {
            view: v,
            parent: None,
            fresh: false,
        }));
        let scroll = |a: &App| a.preview.as_ref().unwrap().view.scroll;
        let pos = |a: &App| a.preview.as_ref().unwrap().view.match_pos();
        assert_eq!(pos(&a), (1, 3));
        // n/N work from RESULTS as well as PREVIEW, and wrap.
        a.on_key(ch('n'));
        assert_eq!((pos(&a), scroll(&a)), ((2, 3), 25));
        a.on_key(ch('l'));
        a.on_key(ch('n'));
        assert_eq!((pos(&a), scroll(&a)), ((3, 3), 45));
        a.on_key(ch('n'));
        assert_eq!((pos(&a), scroll(&a)), ((1, 3), 5));
        a.on_key(ch('N'));
        assert_eq!(pos(&a), (3, 3));
        a.on_key(ch('g'));
        assert_eq!(scroll(&a), 0);
        a.on_key(ch('j'));
        a.on_key(key(KeyCode::PageDown));
        assert_eq!(scroll(&a), 11);
        a.on_key(ch('G'));
        assert_eq!(scroll(&a), 50);
        assert_eq!(a.preview.as_ref().unwrap().view.percent(), 100);
        assert_eq!(a.selected, 0, "preview keys leave the selection alone");
    }

    #[test]
    fn status_cleared_by_any_key() {
        let mut a = app();
        a.status = Some("x".into());
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.status, None);
        assert_eq!(a.selected, 1);
    }
}
