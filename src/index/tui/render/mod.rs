//! Drawing ("Inspector" layout). Reads state, never
//! mutates. Three panes left to right — filters, results, preview — over a
//! one-line footer; overlays (help, modals) on top. Narrow terminals drop
//! the filter pane, then the preview (see [`App::panes`]).

mod filters;
mod footer;
mod overlay;
mod preview;
mod results;

pub use results::snippet_spans;

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType};
use tantivy::DocAddress;

use super::app::{App, Focus, Mode};
use crate::index::live::LiveMap;
use crate::index::query::Row;
use crate::index::snippet::Snippet;
use crate::session::Agent;
use crate::session::homes::user_home;
use crate::session::timefmt::parse_iso;
use crate::theme;

/// Filter pane width (with borders) and the gap between panes.
const FILTERS_WIDTH: u16 = 28;
const GAP: u16 = 1;
/// Results take this share of the width right of the filters ...
const RESULTS_PERCENT: u16 = 40;
/// ... but at least this many columns when there is room.
const RESULTS_MIN: u16 = 36;
/// Card rows above the cards: query line and rule.
const RESULTS_HEAD: u16 = 2;
/// Preview rows around the transcript: header (4), rule, rule, footer.
const PREVIEW_HEADER: u16 = 4;
const PREVIEW_CHROME: u16 = PREVIEW_HEADER + 3;

/// Index data the renderer needs besides [`App`].
pub struct Data<'a> {
    pub rows: &'a [Row],
    pub snippets: &'a HashMap<DocAddress, Snippet>,
    pub live: &'a LiveMap,
    /// Background index refresh in progress.
    pub indexing: bool,
    /// No index exists yet (first run, still building).
    pub building: bool,
}

/// Areas of the main screen (pane rects include their borders).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Areas {
    pub filters: Option<Rect>,
    pub results: Rect,
    pub preview: Option<Rect>,
    pub footer: Rect,
}

/// Main-screen layout for a terminal of `area`.
pub fn areas(area: Rect, app: &App) -> Areas {
    let footer_h = area.height.min(1);
    let body = Rect {
        height: area.height - footer_h,
        ..area
    };
    let footer = Rect {
        y: body.bottom(),
        height: footer_h,
        ..area
    };
    let (show_filters, show_preview) = app.panes(area.width);
    let (mut x, mut w) = (body.x, body.width);
    let filters = show_filters.then(|| {
        let r = Rect {
            x,
            width: FILTERS_WIDTH.min(w),
            ..body
        };
        let used = (r.width + GAP).min(w);
        (x, w) = (x + used, w - used);
        r
    });
    let results_w = if show_preview {
        (w * RESULTS_PERCENT / 100).max(RESULTS_MIN).min(w)
    } else {
        w
    };
    let results = Rect {
        x,
        width: results_w,
        ..body
    };
    let preview = show_preview.then(|| {
        let skip = (results_w + GAP).min(w);
        Rect {
            x: x + skip,
            width: w - skip,
            ..body
        }
    });
    Areas {
        filters,
        results,
        preview,
        footer,
    }
}

/// Inside of a bordered pane.
fn inner(r: Rect) -> Rect {
    r.inner(Margin::new(1, 1))
}

/// Inside of a pane minus one column of padding on each side.
fn padded(r: Rect) -> Rect {
    inner(r).inner(Margin::new(1, 0))
}

/// The card list inside the results pane.
pub fn cards_area(results: Rect) -> Rect {
    let i = inner(results);
    let head = RESULTS_HEAD.min(i.height);
    Rect {
        y: i.y + head,
        height: i.height - head,
        ..i
    }
}

/// Number of 3-line cards that fit in the results pane.
pub fn visible_items(results: Rect) -> usize {
    usize::from(cards_area(results).height) / 3
}

/// Transcript area of a preview-style pane `r`.
fn body_area(r: Rect) -> Rect {
    let p = padded(r);
    let top = (PREVIEW_HEADER + 1).min(p.height);
    Rect {
        y: p.y + top,
        height: p.height.saturating_sub(PREVIEW_CHROME),
        ..p
    }
}

/// Transcript viewport (cells) of the preview pane `r`.
pub fn preview_viewport(r: Rect) -> (usize, usize) {
    let b = body_area(r);
    (usize::from(b.width), usize::from(b.height))
}

/// Full-view pane: everything but the footer.
fn view_area(area: Rect) -> Rect {
    Rect {
        height: area.height.saturating_sub(1),
        ..area
    }
}

/// Transcript viewport of the full-session viewer for a terminal of `area`.
pub fn view_viewport(area: Rect) -> (usize, usize) {
    preview_viewport(view_area(area))
}

/// Draw the whole UI.
pub fn draw(frame: &mut Frame, app: &App, data: &Data) {
    let area = frame.area();
    frame
        .buffer_mut()
        .set_style(area, theme::text().bg(theme::BG));
    let a = areas(area, app);
    if let Mode::View(v) = &app.mode {
        preview::draw_view(frame, app, v, view_area(area));
        footer::draw(frame, app, a.footer);
        return;
    }
    let focused = |f: Focus| match app.mode {
        Mode::Query => f == Focus::Results,
        _ => app.focus == f,
    };
    if let Some(r) = a.filters {
        filters::draw(frame, app, r, focused(Focus::Filters));
    }
    results::draw(frame, app, data, a.results, focused(Focus::Results));
    if let Some(r) = a.preview {
        preview::draw(frame, app, data, r, focused(Focus::Preview));
    }
    footer::draw(frame, app, a.footer);
    overlay::draw(frame, app, data, area);
}

/// Rounded pane border, ACCENT when focused.
fn pane(focused: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border(focused))
        .style(Style::new().bg(theme::BG))
}

/// A horizontal rule across `width` cells.
fn rule(width: u16) -> Line<'static> {
    Line::styled("─".repeat(usize::from(width)), theme::border(false))
}

/// Paint the background of `r` (selected rows/cards).
fn fill(frame: &mut Frame, r: Rect, style: Style) {
    let r = r.intersection(frame.area());
    frame.buffer_mut().set_style(r, style);
}

fn selected_bg() -> Style {
    Style::new().bg(theme::BG_SEL)
}

/// The ACCENT `▌` marking a selected row, or a blank of the same width.
fn bar(on: bool) -> Span<'static> {
    if on {
        Span::styled("▌", theme::heading())
    } else {
        Span::raw(" ")
    }
}

pub(super) fn agent_color(agent: Agent) -> ratatui::style::Color {
    match agent {
        Agent::Claude => theme::CLAUDE,
        Agent::Codex => theme::CODEX,
        Agent::Pi => theme::PI,
        Agent::OpenCode => theme::OPENCODE,
    }
}

/// `CLD` / `CDX` / `PI ` / `OPC` agent badge (3 cells).
fn badge(agent: Agent) -> Span<'static> {
    Span::styled(
        agent.badge(),
        Style::new()
            .fg(agent_color(agent))
            .add_modifier(Modifier::BOLD),
    )
}

/// Replace `$HOME` with `~`.
fn tilde(p: &str) -> String {
    let home = user_home().to_string_lossy().into_owned();
    match p.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => p.to_string(),
    }
}

/// `~/.../<base>` when longer than `max` chars.
fn short_dir(p: &str, max: usize) -> String {
    let t = tilde(p);
    if t.chars().count() > max {
        let base = Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("~/.../{base}")
    } else {
        t
    }
}

fn branch_or_dash(r: &Row) -> &str {
    if r.branch.is_empty() {
        "—"
    } else {
        &r.branch
    }
}

fn local(ts: &str) -> Option<DateTime<Local>> {
    parse_iso(ts).map(|d| d.with_timezone(&Local))
}

/// `N user msgs`: the session's genuine user-message count (`Row::lines`).
fn msgs(r: &Row) -> String {
    match r.lines {
        1 => "1 user msg".into(),
        n => format!("{n} user msgs"),
    }
}

/// Compact age of `modified`: `5m`, `3h`, `2d`, `1w`, `4mo`, `1y`.
fn age(r: &Row) -> String {
    let Some(m) = local(&r.modified) else {
        return "?".into();
    };
    let d = Utc::now().signed_duration_since(m.with_timezone(&Utc));
    let (h, days) = (d.num_hours(), d.num_days());
    if h < 1 {
        format!("{}m", d.num_minutes().max(1))
    } else if h < 24 {
        format!("{h}h")
    } else if days < 7 {
        format!("{days}d")
    } else if days < 30 {
        format!("{}w", days / 7)
    } else if days < 365 {
        format!("{}mo", days / 30)
    } else {
        format!("{}y", days / 365)
    }
}

/// `today 09:58`, `Oct 28`, or `Oct 28 2025` (as the session menu shows it).
fn when(ts: &str, now: DateTime<Local>) -> String {
    match local(ts) {
        Some(t) if t.date_naive() == now.date_naive() => t.format("today %H:%M").to_string(),
        Some(t) if t.format("%Y").to_string() == now.format("%Y").to_string() => {
            t.format("%b %d").to_string()
        }
        Some(t) => t.format("%b %d %Y").to_string(),
        None => ts.chars().take(10).collect(),
    }
}

/// Spans for `text[row]`; each byte takes the style of the last layer
/// whose ranges contain it, else `base`.
fn spans(
    text: &str,
    row: Range<usize>,
    base: Style,
    layers: &[(&[Range<usize>], Style)],
) -> Vec<Span<'static>> {
    let mut cuts = vec![row.start, row.end];
    for (rs, _) in layers {
        for r in rs.iter() {
            cuts.extend([r.start, r.end].into_iter().filter(|b| row.contains(b)));
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .filter(|w| w[0] < w[1])
        .map(|w| {
            let st = layers
                .iter()
                .rev()
                .find(|(rs, _)| rs.iter().any(|r| r.start <= w[0] && w[1] <= r.end))
                .map_or(base, |l| l.1);
            Span::styled(text[w[0]..w[1]].to_string(), st)
        })
        .collect()
}

#[cfg(test)]
mod tests;
