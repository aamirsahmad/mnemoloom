//! Native interactive picker: urgency-ordered rows, type-to-filter over
//! metadata and conversations (see [`matcher`], [`search`]), a live preview
//! of the highlighted pane, and a background rescan.

mod ansi;
mod app;
mod matcher;
mod search;
mod ui;

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};

use self::app::{Action, App};
use super::filters::{View, display};
use super::model::Agent;
use super::{now, tmux};

/// How often the preview pane is recaptured.
const PREVIEW_INTERVAL: Duration = Duration::from_millis(500);
/// Input poll interval; bounds how stale a finished scan or preview can look.
const TICK: Duration = Duration::from_millis(50);
/// Name prefix of the picker's background threads.
const WORKER: &str = "mux-worker";

/// Run the picker over `agents`; returns the pane to jump to, if any.
/// `refresh` starts a background rescan immediately (rows came from cache).
pub fn pick(agents: Vec<Agent>, view: &View, refresh: bool) -> Result<Option<String>> {
    let mut terminal = ratatui::try_init()?;
    quiet_worker_panics();
    let result = event_loop(&mut terminal, App::new(agents), view, refresh);
    ratatui::try_restore()?;
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    mut app: App,
    view: &View,
    refresh: bool,
) -> Result<Option<String>> {
    let (scan_tx, scan_rx) = mpsc::channel();
    let preview = PreviewWorker::spawn();
    if refresh {
        app.scanning = true;
        spawn_scan(scan_tx.clone());
    }
    let index = search::default_index();
    let searcher = index
        .clone()
        .map(|(dir, homes)| search::Searcher::spawn(dir, homes));
    let updated = index.map(|(dir, homes)| search::spawn_update(dir, homes));
    app.indexing = updated.is_some();
    let mut target: Option<String> = None;
    loop {
        if app.selected_pane() != target.as_deref() {
            target = app.selected_pane().map(str::to_owned);
            if let Some(pane) = &target {
                preview.watch(pane.clone());
            }
        }
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match app.on_key(key) {
                Action::Quit => return Ok(None),
                Action::Jump(pane) => return Ok(Some(pane)),
                Action::Refresh if !app.scanning => {
                    app.scanning = true;
                    spawn_scan(scan_tx.clone());
                }
                Action::Refresh | Action::None => {}
            }
        }
        if let Ok(scanned) = scan_rx.try_recv() {
            app.scanning = false;
            match scanned {
                Ok(agents) => app.set_agents(display(agents, view, now())),
                Err(err) => app.error = Some(format!("scan failed: {err}")),
            }
        }
        while let Ok((pane, text)) = preview.results.try_recv() {
            app.set_preview(pane, &text);
        }
        let Some(searcher) = &searcher else {
            continue;
        };
        if let Some(Ok(done)) = updated.as_ref().map(Receiver::try_recv) {
            app.indexing = false;
            match done {
                Ok(stats) if stats.changed() || !app.index_ready => searcher.reload(),
                Ok(_) => {}
                Err(err) => app.error = Some(format!("index update failed: {err}")),
            }
        }
        while let Ok(reply) = searcher.replies.try_recv() {
            match reply {
                search::Reply::Ready(ready) => app.set_index_ready(ready),
                search::Reply::Hits(seq, hits) => {
                    app.set_hits(seq, hits);
                }
                search::Reply::Failed(err) => app.error = Some(err),
            }
        }
        if let Some(request) = app.search_due(Instant::now()) {
            searcher.query(request);
        }
    }
}

/// ratatui's panic hook restores the terminal for any thread. A worker panic
/// is caught and shown in the UI instead, so the hook must leave the terminal
/// alone (and print nothing) for worker threads.
fn quiet_worker_panics() {
    let restore_and_report = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let worker = thread::current()
            .name()
            .is_some_and(|n| n.starts_with(WORKER));
        if !worker {
            restore_and_report(info);
        }
    }));
}

/// Run `body` on a named worker thread.
fn spawn_worker(role: &str, body: impl FnOnce() + Send + 'static) {
    // Without the thread the picker still works, minus refresh or preview.
    let _ = thread::Builder::new()
        .name(format!("{WORKER}-{role}"))
        .spawn(body);
}

/// `body`'s result, or its panic message.
fn guarded<T>(body: impl FnOnce() -> T) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(body)).map_err(|payload| panic_message(&*payload))
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_owned())
}

/// A fresh scan (which also rewrites the cache) on its own thread.
fn spawn_scan(tx: Sender<Result<Vec<Agent>, String>>) {
    spawn_worker("scan", move || {
        let _ = tx.send(guarded(super::fresh));
    });
}

/// Recaptures the watched pane every `PREVIEW_INTERVAL`, and at once when the
/// watched pane changes. Stops when the picker drops it.
struct PreviewWorker {
    watch: Sender<String>,
    results: Receiver<(String, String)>,
}

impl PreviewWorker {
    fn spawn() -> Self {
        let (watch, targets) = mpsc::channel::<String>();
        let (out, results) = mpsc::channel();
        spawn_worker("preview", move || {
            let mut pane: Option<String> = None;
            loop {
                match targets.recv_timeout(PREVIEW_INTERVAL) {
                    Ok(next) => pane = Some(targets.try_iter().last().unwrap_or(next)),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                if let Some(p) = &pane {
                    let text = guarded(|| tmux::preview(p))
                        .unwrap_or_else(|err| format!("preview failed: {err}"));
                    if out.send((p.clone(), text)).is_err() {
                        return;
                    }
                }
            }
        });
        PreviewWorker { watch, results }
    }

    fn watch(&self, pane: String) {
        let _ = self.watch.send(pane);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarded_reports_panic_messages() {
        assert_eq!(guarded(|| 7), Ok(7));
        let handle = thread::Builder::new()
            .name(format!("{WORKER}-test"))
            .spawn(|| guarded(|| -> u8 { panic!("boom {}", 1) }))
            .unwrap();
        assert_eq!(handle.join().unwrap(), Err("boom 1".to_owned()));
    }
}
