//! Rendering ("Inspector" layout): a SESSION pane (header + first/last
//! user message) beside an ACTIONS pane holding the current list or form,
//! a one-line footer (mode chip + key hints) and the `?` help overlay.
//!
//! Below [`NARROW`] columns the SESSION pane is hidden; the ACTIONS pane
//! title then carries the agent badge and short id instead.

mod actions;
mod dialogs;
mod footer;
mod forms;
mod session_pane;
#[cfg(test)]
mod tests;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph};

use self::actions::draw_main;
use self::footer::{draw_help, footer_line};
use self::session_pane::draw_session;
use super::session::SessionView;
use super::state::{Screen, State};
use crate::index::tui::text::wrap_text;
use crate::session::Agent;
use crate::theme;

/// Narrower terminals hide the SESSION pane.
pub const NARROW: u16 = 70;
/// Selected-row marker.
const BAR: &str = "▌";

/// Static context shown alongside the state.
pub struct Ctx<'a> {
    pub sessions: &'a [SessionView],
    /// Results picker title (e.g. ` Select a session `).
    pub results_title: &'a str,
    /// `Latest ... sessions for '<project>'...`.
    pub scope_line: Option<&'a str>,
}

pub fn draw(frame: &mut Frame, state: &State, ctx: &Ctx) {
    let area = frame.area();
    frame.render_widget(Block::new().style(Style::new().bg(theme::BG)), area);
    let [body, footer] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let session = shown_session(state, ctx);
    let main = match session {
        Some(s) if area.width >= NARROW => {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Fill(1)]).areas(body);
            draw_session(frame, left, s);
            right
        }
        _ => body,
    };
    let compact = session.filter(|_| area.width < NARROW && state.screen != Screen::Results);
    draw_main(frame, main, state, ctx, compact);
    frame.render_widget(Paragraph::new(footer_line(state, footer.width)), footer);
    if state.help {
        draw_help(frame, body, state);
    }
}

/// The session the SESSION pane describes (the hovered one on results).
fn shown_session<'a>(state: &State, ctx: &Ctx<'a>) -> Option<&'a SessionView> {
    match state.screen {
        Screen::TrimConfirm(_) | Screen::DirConfirm(_) => None,
        Screen::Results => ctx.sessions.get(state.cursor),
        _ => ctx.sessions.get(state.current),
    }
}

fn pane(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border(focused))
        .title(title)
        .padding(Padding::horizontal(1))
}

fn bold(style: Style) -> Style {
    style.add_modifier(Modifier::BOLD)
}

/// `CLD` / `CDX` / `PI ` / `OPC` in the agent colour.
fn agent_badge(agent: Agent) -> Span<'static> {
    let color = match agent {
        Agent::Claude => theme::CLAUDE,
        Agent::Codex => theme::CODEX,
        Agent::Pi => theme::PI,
        Agent::OpenCode => theme::OPENCODE,
    };
    Span::styled(agent.badge(), bold(Style::new().fg(color)))
}

fn short_id(s: &SessionView) -> String {
    s.session_id.chars().take(8).collect()
}

fn dim(text: impl Into<String>) -> Line<'static> {
    Line::styled(text.into(), theme::dim())
}

fn heading(text: &str) -> Line<'static> {
    Line::styled(text.to_string(), theme::heading())
}

/// `text` wrapped to `width` cells (at most `max` lines), each line styled.
fn wrapped(text: &str, width: usize, max: usize, style: Style) -> Vec<Line<'static>> {
    wrap_text(text, width.max(1))
        .into_iter()
        .take(max)
        .map(|l| Line::styled(l, style))
        .collect()
}
