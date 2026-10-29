//! Terminal driver: draws [`State`] with `ui`, feeds keys to the reducer,
//! and fills in session [`Details`] from a background thread.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event};

use super::session::{Details, SessionView, load_details};
use super::state::{Action, Key, Screen, State, Step};
use super::ui::{self, Ctx};
use crate::exit::Exit;
use crate::session::Homes;

/// Input poll interval while details are still loading.
const TICK: Duration = Duration::from_millis(30);

/// Run the menu over `sessions`; returns the chosen session index and
/// action, or `None` if the user backed out. The terminal is restored
/// before returning.
pub fn run(
    state: State,
    sessions: &mut [SessionView],
    homes: &Homes,
    results_title: &str,
    scope_line: Option<&str>,
) -> anyhow::Result<Option<(usize, Action)>> {
    let rx = spawn_details(sessions, homes);
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(
        &mut terminal,
        state,
        sessions,
        rx,
        results_title,
        scope_line,
    );
    ratatui::try_restore()?;
    result
}

/// Run a standalone dialog screen (trim-confirm / dir-confirm) and return
/// the [`Action`] it produced.
pub fn run_dialog(screen: Screen) -> anyhow::Result<Option<Action>> {
    let (_tx, rx) = mpsc::channel();
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(&mut terminal, State::dialog(screen), &mut [], rx, "", None);
    ratatui::try_restore()?;
    Ok(result?.map(|(_, action)| action))
}

fn spawn_details(sessions: &[SessionView], homes: &Homes) -> mpsc::Receiver<(usize, Details)> {
    let (tx, rx) = mpsc::channel();
    for (i, s) in sessions.iter().enumerate() {
        let (tx, s, homes) = (tx.clone(), s.clone(), homes.clone());
        thread::spawn(move || {
            let _ = tx.send((i, load_details(&s, &homes)));
        });
    }
    rx
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    mut state: State,
    sessions: &mut [SessionView],
    rx: mpsc::Receiver<(usize, Details)>,
    results_title: &str,
    scope_line: Option<&str>,
) -> anyhow::Result<Option<(usize, Action)>> {
    let mut pending = sessions.len();
    loop {
        let ctx = Ctx {
            sessions,
            results_title,
            scope_line,
        };
        terminal.draw(|f| ui::draw(f, &state, &ctx))?;
        let timeout = if pending > 0 {
            TICK
        } else {
            Duration::from_secs(3600)
        };
        if event::poll(timeout)?
            && let Event::Key(k) = event::read()?
            && let Some(key) = Key::from_event(k)
        {
            match state.handle(key, sessions) {
                Step::Stay => {}
                Step::Quit => return Ok(None),
                Step::Interrupt => return Err(Exit::code(130).into()),
                Step::Run(action) => return Ok(Some((state.current, action))),
            }
        }
        while let Ok((i, d)) = rx.try_recv() {
            sessions[i].details = Some(d);
            pending -= 1;
        }
    }
}
