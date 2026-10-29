//! Results pane: the query line, then one 3-line card per session.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{
    Data, age, badge, bar, branch_or_dash, cards_area, fill, inner, msgs, padded, pane, rule,
    selected_bg, spans,
};
use crate::index::query::Row;
use crate::index::snippet::Snippet;
use crate::index::tui::app::{App, Mode};
use crate::index::tui::text::{self, fit, suffix, truncate, wrap_ranges};
use crate::session::Derivation;
use crate::session::message::display_title;
use crate::theme;

/// Cells before a card's 2nd/3rd line (bar, space, badge, space).
const CARD_INDENT: usize = 6;

/// Draw the results pane into `r`.
pub fn draw(frame: &mut Frame, app: &App, data: &Data, r: Rect, focused: bool) {
    frame.render_widget(pane(focused), r);
    let head = padded(r);
    if head.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(query_line(app, data, usize::from(head.width))),
        Rect { height: 1, ..head },
    );
    let i = inner(r);
    if i.height > 1 {
        frame.render_widget(
            Paragraph::new(rule(i.width)),
            Rect {
                y: i.y + 1,
                height: 1,
                ..i
            },
        );
    }
    let list = cards_area(r);
    if app.results.is_empty() {
        let msg = if data.building {
            "Building index…"
        } else if app.query.is_empty() {
            "No sessions"
        } else {
            "No results"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, theme::dim())),
            list.inner(ratatui::layout::Margin::new(1, 0)),
        );
        return;
    }
    let end = (app.list_scroll + list.height as usize / 3).min(app.results.len());
    for (n, pos) in (app.list_scroll..end).enumerate() {
        let row = &data.rows[app.results[pos]];
        let card = Rect {
            y: list.y + 3 * n as u16,
            height: 3,
            ..list
        };
        let sel = pos == app.selected;
        if sel {
            fill(frame, card, selected_bg());
        }
        let lines = card_lines(app, data, row, sel, usize::from(card.width));
        frame.render_widget(Paragraph::new(lines), card);
    }
}

/// `/ query▏ … N hits · relevance`.
fn query_line(app: &App, data: &Data, width: usize) -> Line<'static> {
    let mut right = if app.query.is_empty() {
        format!("{} sessions", app.results.len())
    } else {
        let sort = if app.by_time { "time" } else { "relevance" };
        format!("{} hits · {sort}", app.results.len())
    };
    if data.indexing || data.building {
        right.push_str(" · indexing…");
    }
    let typing = app.mode == Mode::Query;
    let room = width.saturating_sub(text::width(&right) + 4);
    let mut v = vec![Span::styled(
        "/ ",
        theme::heading().add_modifier(Modifier::BOLD),
    )];
    if app.query.is_empty() && !typing {
        v.push(Span::styled(
            truncate("press / to search", room),
            theme::dim(),
        ));
    } else {
        v.push(Span::styled(
            suffix(&app.query, room.saturating_sub(1)).to_string(),
            theme::text(),
        ));
    }
    if typing {
        v.push(Span::styled("▏", theme::heading()));
    }
    let used: usize = v.iter().map(Span::width).sum();
    let right = truncate(&right, width.saturating_sub(used + 1));
    v.push(Span::raw(
        " ".repeat(width.saturating_sub(used + text::width(&right))),
    ));
    v.push(Span::styled(right, theme::dim()));
    Line::from(v)
}

/// Title: custom title, else the first (user) message ([`display_title`]).
pub(super) fn title(r: &Row) -> String {
    if r.custom_title.is_empty() {
        display_title(r.first_msg())
    } else {
        r.custom_title.clone()
    }
}

/// ` · trimmed` etc. for derived / sub-agent / exec sessions.
fn kind_tag(r: &Row) -> &'static str {
    if r.is_sidechain {
        " · sub-agent"
    } else if r.is_exec_run {
        " · exec"
    } else if r.derivation_type == Derivation::Trimmed.as_str() {
        " · trimmed"
    } else if r.derivation_type == Derivation::Continued.as_str() {
        " · rollover"
    } else {
        ""
    }
}

fn card_lines(app: &App, data: &Data, r: &Row, sel: bool, width: usize) -> Vec<Line<'static>> {
    let room = width.saturating_sub(CARD_INDENT);
    // 1: badge, title, age.
    let age = age(r);
    let title_w = room.saturating_sub(text::width(&age) + 1);
    let first = Line::from(vec![
        bar(sel),
        Span::raw(" "),
        badge(r.agent),
        Span::raw(" "),
        Span::styled(
            fit(&flat(&title(r)), title_w, false),
            theme::text().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(age, theme::dim()),
    ]);
    // 2: project @ branch · N lines [· ● live].
    let live = data.live.contains_key(&r.file_path);
    let live_w = if live { 9 } else { 0 };
    // Cut `project @ branch` first so the count and tags stay visible.
    let tail = format!(" · {}{}", msgs(r), kind_tag(r));
    let place = format!("{} @ {}", r.project_name(), branch_or_dash(r));
    let place_w = room.saturating_sub(live_w + text::width(&tail)).max(8);
    let place = truncate(
        &format!("{}{tail}", truncate(&place, place_w)),
        room.saturating_sub(live_w),
    );
    let mut second = vec![
        bar(sel),
        Span::raw(" ".repeat(CARD_INDENT - 1)),
        Span::styled(place, theme::dim()),
    ];
    if live {
        second.extend([
            Span::styled(" · ", theme::dim()),
            Span::styled("●", Style::new().fg(theme::OK)),
            Span::styled(" live", theme::dim()),
        ]);
    }
    // 3: …snippet with the matched terms highlighted.
    let soft = Style::new().fg(theme::FG_SOFT);
    let mut third = vec![bar(sel), Span::raw(" ".repeat(CARD_INDENT - 1))];
    third.push(Span::styled("…", soft));
    let max = room.saturating_sub(1);
    match data.snippets.get(&r.doc).filter(|_| !app.query.is_empty()) {
        Some(s) => third.extend(snippet_spans(s, max, soft, theme::hit())),
        None => {
            let text = if app.query.is_empty() {
                r.last_msg_content.clone()
            } else {
                display_title(r.first_msg())
            };
            third.push(Span::styled(truncate(&flat(&text), max), soft));
        }
    }
    vec![first, Line::from(second), Line::from(third)]
}

/// Newlines and tabs as spaces (single-line display).
fn flat(s: &str) -> String {
    s.replace(['\n', '\r', '\t'], " ")
}

/// A snippet on one line, cut to `max` cells at a word boundary.
pub fn snippet_spans(s: &Snippet, max: usize, base: Style, hl: Style) -> Vec<Span<'static>> {
    let flat = flat(&s.text);
    let row = wrap_ranges(&flat, max).swap_remove(0);
    spans(&flat, row, base, &[(&s.highlights, hl)])
}
