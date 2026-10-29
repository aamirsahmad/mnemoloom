//! Interactive search UI (ratatui). The first frame is drawn from the
//! existing index while an incremental refresh runs on a background
//! thread; live-session scans also run in the background every 2 s, and
//! the selected session's transcript is loaded there for the preview.
//!
//! Row indices (`App::results`) are only valid for the catalog they were
//! computed from: every catalog reload goes through [`Model::reload`], which
//! remembers the selection by file path and recomputes the results.

pub mod actions;
pub mod app;
pub mod facets;
pub mod render;
pub mod text;
pub mod view;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;
use tantivy::DocAddress;

use super::build::{self, UpdateStats};
use super::live::{self, Candidate, LiveMap};
use super::query::{self, Catalog, Filters, Row, TextQuery};
use super::snippet::{Snippet, keywords};
use super::store::LockWait;
use crate::cli::GlobalArgs;
use crate::session::Homes;
use crate::session::discover::current_git_branch;
use app::{Action, App, Effect, Mode};
use view::{Preview, ViewState};

const DEBOUNCE: Duration = Duration::from_millis(200);
const LIVE_EVERY: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(50);

/// Everything the TUI starts from.
pub struct Launch {
    pub global: GlobalArgs,
    pub homes: Homes,
    pub index_dir: PathBuf,
    pub filters: Filters,
    pub query: String,
    pub by_time: bool,
    pub limit: Option<usize>,
}

/// Index-side state: catalog, current query, snippet cache, live map.
struct Model {
    cat: Option<Catalog>,
    query: Option<TextQuery>,
    snippets: HashMap<DocAddress, Snippet>,
    live: LiveMap,
    /// Live-detection input, rebuilt only when the catalog changes.
    candidates: Arc<Vec<Candidate>>,
}

impl Model {
    fn new(cat: Option<Catalog>) -> Model {
        let mut m = Model {
            cat,
            query: None,
            snippets: HashMap::new(),
            live: LiveMap::new(),
            candidates: Arc::default(),
        };
        m.candidates = Arc::new(live::candidates(m.rows()));
        m
    }

    fn rows(&self) -> &[Row] {
        self.cat.as_ref().map_or(&[], |c| c.rows.as_slice())
    }

    fn selected_path(&self, app: &App) -> Option<String> {
        app.selected_row()
            .and_then(|i| self.rows().get(i))
            .map(|r| r.file_path.clone())
    }

    /// Re-run the search. With `keep`, stay on the same session if present.
    fn refilter(&mut self, app: &mut App, keep: bool) {
        let prev = keep.then(|| self.selected_path(app)).flatten();
        self.refilter_to(app, prev);
    }

    fn refilter_to(&mut self, app: &mut App, prev: Option<String>) {
        self.snippets.clear();
        let Some(cat) = &self.cat else {
            app.set_results(Vec::new());
            app.set_counts(facets::Counts::default());
            return;
        };
        self.query = (!app.query.trim().is_empty())
            .then(|| query::run_query(cat, &app.query, &app.filters, SystemTime::now()));
        let live: HashSet<String> = self.live.keys().cloned().collect();
        // One pass with the agent facet relaxed: the facet counts come from
        // it (each facet counts with the others applied), the results by
        // filtering it in memory. The limit applies last, so it doesn't skew
        // the counts or leave room unfilled.
        let relaxed = Filters {
            agents: query::Agents::ALL,
            ..app.filters.clone()
        };
        let q = self.query.as_ref();
        let mut sel = query::select(cat, &relaxed, q, app.by_time, None, Some(&live));
        app.set_counts(facets::count(&cat.rows, &sel, &app.filters, &app.projects));
        sel.retain(|&i| {
            let r = &cat.rows[i];
            app.filters.agents.contains(r.agent) && app.projects.shows(r.project_name())
        });
        if let Some(n) = app.limit {
            sel.truncate(n);
        }
        app.set_results(sel);
        if let Some(p) = app.preview.as_mut()
            && p.view.set_keywords(keywords(&app.query))
        {
            p.fresh = true;
        }
        if let Some(pos) =
            prev.and_then(|p| app.results.iter().position(|&i| cat.rows[i].file_path == p))
        {
            app.selected = pos;
        }
    }

    /// Reload the catalog (opening it if needed) and recompute results,
    /// keeping the selection by file path.
    fn reload(&mut self, app: &mut App, dir: &Path, homes: &Homes) -> anyhow::Result<()> {
        let prev = self.selected_path(app);
        let result = match self.cat.as_mut() {
            Some(c) => c.refresh(),
            None => Catalog::open(dir, homes).map(|c| self.cat = c),
        };
        self.candidates = Arc::new(live::candidates(self.rows()));
        self.refilter_to(app, prev);
        result
    }

    /// Apply a live scan; re-filter only when `--live` is on and the set changed.
    fn set_live(&mut self, app: &mut App, map: LiveMap) {
        let changed = map.keys().collect::<HashSet<_>>() != self.live.keys().collect();
        self.live = map;
        if app.filters.live_only && changed {
            self.refilter(app, true);
        }
    }

    /// Compute snippets for the visible results.
    fn ensure_snippets(&mut self, app: &App, visible: usize) {
        let (Some(cat), Some(q)) = (&self.cat, &self.query) else {
            return;
        };
        let searcher = cat.searcher();
        let end = (app.list_scroll + visible).min(app.results.len());
        for &i in &app.results[app.list_scroll.min(end)..end] {
            let doc = cat.rows[i].doc;
            self.snippets
                .entry(doc)
                .or_insert_with(|| q.snippet(cat, &searcher, doc));
        }
    }
}

enum Job {
    Index(anyhow::Result<UpdateStats>),
    Live(LiveMap),
    Preview(Box<Preview>),
}

/// Background jobs: channel plus in-flight flags. Reindex requests made
/// while one runs are coalesced into a single follow-up run.
struct Bg {
    tx: mpsc::Sender<Job>,
    rx: Receiver<Job>,
    indexing: bool,
    index_again: bool,
    live_pending: bool,
    /// A preview load is running.
    preview_pending: bool,
}

impl Bg {
    fn new() -> Bg {
        let (tx, rx) = mpsc::channel();
        Bg {
            tx,
            rx,
            indexing: false,
            index_again: false,
            live_pending: false,
            preview_pending: false,
        }
    }

    fn index(&mut self, l: &Launch) {
        if self.indexing {
            self.index_again = true;
            return;
        }
        self.indexing = true;
        let (dir, homes, tx) = (l.index_dir.clone(), l.homes.clone(), self.tx.clone());
        thread::spawn(move || {
            // Waits for another process's build; this thread is background.
            let _ = tx.send(Job::Index(build::update(
                &dir,
                &homes,
                None,
                LockWait::Block,
            )));
        });
    }

    /// The running index job finished; start the queued one, if any.
    fn index_done(&mut self, l: &Launch) {
        self.indexing = false;
        if std::mem::take(&mut self.index_again) {
            self.index(l);
        }
    }

    /// Load `row`'s transcript; a read error becomes the preview's text.
    fn preview(&mut self, row: Row, kws: Vec<String>) {
        self.preview_pending = true;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let p = Preview::load(row.clone(), kws.clone()).unwrap_or_else(|e| Preview {
                view: ViewState::new(row, vec![format!("Cannot read session: {e:#}")], kws),
                parent: None,
                fresh: false,
            });
            let _ = tx.send(Job::Preview(Box::new(p)));
        });
    }

    fn live(&mut self, cands: &Arc<Vec<Candidate>>) {
        self.live_pending = true;
        let (cands, tx) = (Arc::clone(cands), self.tx.clone());
        thread::spawn(move || {
            let _ = tx.send(Job::Live(live::scan(&cands)));
        });
    }
}

/// Run the TUI until quit.
pub fn run(l: Launch) -> anyhow::Result<ExitCode> {
    let mut model = Model::new(Catalog::open(&l.index_dir, &l.homes).unwrap_or(None));
    let mut app = App::new(l.filters.clone(), l.query.clone(), l.by_time, l.limit);
    app.current_branch = current_git_branch(Path::new(&app.filters.launch_cwd));
    model.refilter(&mut app, false);
    let mut terminal = ratatui::try_init()?;
    let mut bg = Bg::new();
    bg.index(&l);
    bg.live(&model.candidates);

    let result = event_loop(&mut terminal, &l, &mut app, &mut model, &mut bg);
    ratatui::restore();
    result
}

fn on_job(job: Job, l: &Launch, app: &mut App, model: &mut Model, bg: &mut Bg) {
    match job {
        Job::Index(Ok(stats)) => {
            bg.index_done(l);
            if let Some(n) = stats.notes.first() {
                app.status = Some(n.clone());
            }
            if (stats.changed() || model.cat.is_none())
                && let Err(e) = model.reload(app, &l.index_dir, &l.homes)
            {
                app.status = Some(format!("Index reload failed: {e:#}"));
            }
        }
        Job::Index(Err(e)) => {
            bg.index_done(l);
            app.status = Some(format!("Index update failed: {e:#}"));
        }
        Job::Live(map) => {
            bg.live_pending = false;
            model.set_live(app, map);
        }
        Job::Preview(mut p) => {
            bg.preview_pending = false;
            // The query may have changed while it loaded.
            p.view.set_keywords(keywords(&app.query));
            app.preview = Some(p);
        }
    }
}

/// Start loading the selected session's preview unless it is loaded (and
/// unchanged) or a load is already running; a stale result triggers
/// another load on the next pass.
fn request_preview(app: &App, model: &Model, bg: &mut Bg) {
    if bg.preview_pending {
        return;
    }
    let Some(row) = app.selected_row().and_then(|i| model.rows().get(i)) else {
        return;
    };
    let current = app
        .preview
        .as_ref()
        .is_some_and(|p| p.path() == row.file_path && p.view.row.modified_ts == row.modified_ts);
    if !current {
        bg.preview(row.clone(), keywords(&app.query));
    }
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    l: &Launch,
    app: &mut App,
    model: &mut Model,
    bg: &mut Bg,
) -> anyhow::Result<ExitCode> {
    let mut pending: Option<Instant> = None;
    let mut last_live = Instant::now();
    let mut dirty = true;
    loop {
        while let Ok(job) = bg.rx.try_recv() {
            dirty = true;
            on_job(job, l, app, model, bg);
        }
        if !bg.live_pending && last_live.elapsed() >= LIVE_EVERY {
            last_live = Instant::now();
            bg.live(&model.candidates);
        }
        if pending.is_some_and(|t| t.elapsed() >= DEBOUNCE) {
            pending = None;
            model.refilter(app, false);
            dirty = true;
        }
        request_preview(app, model, bg);
        if dirty {
            draw(terminal, app, model, bg)?;
            dirty = false;
        }
        if !event::poll(POLL)? {
            continue;
        }
        while event::poll(Duration::ZERO)? {
            dirty = true;
            let Event::Key(k) = event::read()? else {
                continue;
            };
            if k.kind != KeyEventKind::Press {
                continue;
            }
            match app.on_key(k) {
                Effect::None => {}
                Effect::Debounce => pending = Some(Instant::now()),
                Effect::Refilter => {
                    pending = None;
                    model.refilter(app, false);
                }
                Effect::Quit => return Ok(ExitCode::SUCCESS),
                Effect::Run(a) => {
                    if run_action(terminal, l, app, model, a)? {
                        bg.index(l);
                    }
                }
            }
        }
    }
}

fn draw(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    model: &mut Model,
    bg: &Bg,
) -> anyhow::Result<()> {
    let size = terminal.size()?;
    let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
    let visible = layout(app, area);
    app.ensure_visible(visible);
    model.ensure_snippets(app, visible);
    let data = render::Data {
        rows: model.rows(),
        snippets: &model.snippets,
        live: &model.live,
        indexing: bg.indexing,
        building: bg.indexing && model.cat.is_none(),
    };
    terminal.draw(|f| render::draw(f, app, &data))?;
    Ok(())
}

/// Size the panes' state for a terminal of `area`; returns how many result
/// cards fit.
fn layout(app: &mut App, area: ratatui::layout::Rect) -> usize {
    app.layout(area.width);
    let a = render::areas(area, app);
    if let Mode::View(v) = &mut app.mode {
        let (w, h) = render::view_viewport(area);
        v.layout(w, h);
    }
    if let (Some(p), Some(r)) = (app.preview.as_mut(), a.preview) {
        let (w, h) = render::preview_viewport(r);
        p.view.layout(w, h);
        if std::mem::take(&mut p.fresh) {
            p.view.seek_first();
        }
    }
    render::visible_items(a.results)
}

/// Execute an action; returns whether the index should be refreshed.
fn run_action(
    terminal: &mut DefaultTerminal,
    l: &Launch,
    app: &mut App,
    model: &mut Model,
    a: Action,
) -> anyhow::Result<bool> {
    let Some(row) = app
        .selected_row()
        .and_then(|i| model.rows().get(i))
        .cloned()
    else {
        return Ok(false);
    };
    if let Some(msg) = actions::refusal(a, &row) {
        app.status = Some(msg);
        return Ok(false);
    }
    match actions::kind(a) {
        actions::Kind::Inline if a == Action::View => {
            match view::load_text(Path::new(&row.file_path), row.agent) {
                Ok(lines) => {
                    let kws = keywords(&app.query);
                    app.mode = Mode::View(Box::new(ViewState::new(row, lines, kws)));
                }
                Err(e) => app.status = Some(format!("Cannot read session: {e:#}")),
            }
            Ok(false)
        }
        actions::Kind::Inline => {
            app.status = actions::inline_status(a, &row);
            Ok(false)
        }
        _ => {
            ratatui::restore();
            let out = actions::run_external(a, &row, &l.global, &l.homes);
            *terminal = ratatui::try_init()?;
            terminal.clear()?;
            app.status = out.status;
            if out.reindex && a == Action::Delete {
                // Hide the deleted row now; the queued refresh prunes it.
                let rows = model.rows();
                app.results.retain(|&r| rows[r].file_path != row.file_path);
                app.selected = app.selected.min(app.results.len().saturating_sub(1));
            }
            Ok(out.reindex)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    const IDS: [&str; 3] = [
        "c1a0de00-0001-4abc-8def-012345678911",
        "c1a0de00-0002-4abc-8def-012345678912",
        "c1a0de00-0003-4abc-8def-012345678913",
    ];

    fn write(h: &Homes, id: &str, ts: &str) -> PathBuf {
        let lines: Vec<String> = claude_lines(id, "/w")
            .into_iter()
            .map(|l| l.replace("2026-01-03T03:30:00.000Z", ts))
            .collect();
        write_claude(&h.claude, id, "/w", &lines)
    }

    #[test]
    fn reload_keeps_selection_by_path_when_rows_shrink_or_resort() {
        let root = tempfile::tempdir().unwrap();
        let homes = Homes::new(root.path().join("c"), root.path().join("x")).canonical();
        let idx = root.path().join("idx");
        let paths: Vec<PathBuf> = IDS
            .iter()
            .zip([
                "2026-10-03T00:00:00Z",
                "2026-10-02T00:00:00Z",
                "2026-10-01T00:00:00Z",
            ])
            .map(|(id, ts)| write(&homes, id, ts))
            .collect();
        build::update(&idx, &homes, None, LockWait::Block).unwrap();
        let mut model = Model::new(Catalog::open(&idx, &homes).unwrap());
        let mut app = App::new(
            Filters::new(&homes, "/w".into()),
            String::new(),
            false,
            None,
        );
        model.refilter(&mut app, false);
        assert_eq!(app.results.len(), 3);
        // Select the oldest (last) row, then delete the newest file and make
        // the middle one the newest: rows shrink and re-sort.
        app.selected = 2;
        let want = model.selected_path(&app).unwrap();
        std::fs::remove_file(&paths[0]).unwrap();
        write(&homes, IDS[1], "2026-10-09T00:00:00Z");
        build::update(&idx, &homes, None, LockWait::Block).unwrap();
        model.reload(&mut app, &idx, &homes).unwrap();
        assert_eq!(app.results.len(), 2);
        assert_eq!(model.selected_path(&app).unwrap(), want);
        // Selection on a pruned row falls back to the top, without panicking.
        app.selected = 0;
        std::fs::remove_file(&paths[1]).unwrap();
        build::update(&idx, &homes, None, LockWait::Block).unwrap();
        model.reload(&mut app, &idx, &homes).unwrap();
        assert_eq!((app.results.len(), app.selected), (1, 0));
        assert_eq!(model.selected_path(&app).unwrap(), want);
    }

    #[test]
    fn limit_applies_after_counts_and_project_filter() {
        let root = tempfile::tempdir().unwrap();
        let homes = Homes::new(root.path().join("c"), root.path().join("x")).canonical();
        let idx = root.path().join("idx");
        for (id, cwd, ts) in [
            (IDS[0], "/w/a", "2026-10-03T00:00:00Z"),
            (IDS[1], "/w/b", "2026-10-02T00:00:00Z"),
            (IDS[2], "/w/b", "2026-10-01T00:00:00Z"),
        ] {
            let lines: Vec<String> = claude_lines(id, cwd)
                .into_iter()
                .map(|l| l.replace("2026-01-03T03:30:00.000Z", ts))
                .collect();
            write_claude(&homes.claude, id, cwd, &lines);
        }
        build::update(&idx, &homes, None, LockWait::Block).unwrap();
        let mut model = Model::new(Catalog::open(&idx, &homes).unwrap());
        let mut f = Filters::new(&homes, "/w".into());
        f.global = true;
        let mut app = App::new(f, String::new(), true, Some(1));
        model.refilter(&mut app, false);
        assert_eq!(app.results.len(), 1);
        // Counts see every match, not just the first `limit`.
        assert_eq!(app.counts.claude, 3);
        let names: Vec<&str> = app.counts.projects.iter().map(|p| p.0.as_str()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        // Hiding the top project still fills the limit from the rest.
        let top = model.rows()[app.results[0]].project_name().to_string();
        app.projects.hidden.insert(top.clone());
        model.refilter(&mut app, false);
        assert_eq!(app.results.len(), 1);
        assert_ne!(model.rows()[app.results[0]].project_name(), top);
        assert_eq!(app.counts.claude, 2);
        app.limit = None;
        model.refilter(&mut app, false);
        assert_eq!(app.results.len(), 2);
    }

    #[test]
    fn reindex_requests_are_coalesced_not_dropped() {
        let root = tempfile::tempdir().unwrap();
        let homes = Homes::new(root.path().join("c"), root.path().join("x")).canonical();
        write(&homes, IDS[0], "2026-10-01T00:00:00Z");
        let l = Launch {
            global: GlobalArgs::default(),
            homes: homes.clone(),
            index_dir: root.path().join("idx"),
            filters: Filters::new(&homes, "/w".into()),
            query: String::new(),
            by_time: false,
            limit: None,
        };
        let mut bg = Bg::new();
        bg.index(&l);
        // A second request while running is queued, not spawned or dropped.
        write(&homes, IDS[1], "2026-10-02T00:00:00Z");
        bg.index(&l);
        assert!(bg.indexing && bg.index_again);
        let wait = |bg: &Bg| match bg.rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            Job::Index(r) => r.unwrap(),
            Job::Live(_) | Job::Preview(_) => panic!("unexpected job"),
        };
        let first = wait(&bg);
        bg.index_done(&l);
        assert!(bg.indexing && !bg.index_again);
        let second = wait(&bg);
        bg.index_done(&l);
        assert!(!bg.indexing);
        // Between them, both sessions got indexed (the second run saw the
        // file written after the first started, if the first missed it).
        assert_eq!(first.indexed + second.indexed, 2);
    }
}
