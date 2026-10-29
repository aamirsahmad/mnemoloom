//! Preview pane and the full-session viewer: a header block (title, meta,
//! cwd, lineage), the transcript with a right-aligned role gutter and
//! highlighted matches, and a footer with the match counter and a scroll
//! gauge.

use std::ops::Range;

use chrono::Local;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::results::title;
use super::{
    Data, PREVIEW_HEADER, agent_color, body_area, branch_or_dash, msgs, padded, pane, rule, spans,
    tilde, when,
};
use crate::index::query::Row;
use crate::index::snippet::{find_ci, keyword_ranges};
use crate::index::tui::app::App;
use crate::index::tui::text::{self, truncate};
use crate::index::tui::view::{GUTTER, Kind, ViewState, label};
use crate::session::Derivation;
use crate::theme;

/// Cells of the scroll gauge (`▮▮▮▯▯▯▯▯`).
const GAUGE: usize = 8;

/// Draw the preview of the selected session into `r`.
pub fn draw(frame: &mut Frame, app: &App, data: &Data, r: Rect, focused: bool) {
    frame.render_widget(pane(focused), r);
    let Some(row) = app.selected_row().map(|i| &data.rows[i]) else {
        return;
    };
    let preview = app.preview.as_deref().filter(|p| p.path() == row.file_path);
    let view = preview.map(|p| &p.view);
    let parent = preview.and_then(|p| p.parent.as_ref());
    let foot = view.map_or_else(Line::default, |v| footer(v, false, padded(r).width));
    frame_parts(frame, r, row, view, parent, foot);
    if view.is_none() {
        frame.render_widget(
            Paragraph::new(Span::styled("loading…", theme::dim())),
            body_area(r),
        );
    }
}

/// Draw the full-session viewer into `area`.
pub fn draw_view(frame: &mut Frame, app: &App, v: &ViewState, area: Rect) {
    frame.render_widget(pane(true), area);
    let parent = app
        .preview
        .as_deref()
        .filter(|p| p.path() == v.row.file_path)
        .and_then(|p| p.parent.as_ref());
    let foot = footer(v, true, padded(area).width);
    frame_parts(frame, area, &v.row, Some(v), parent, foot);
}

/// Header, rules, transcript and footer inside the bordered pane `r`.
fn frame_parts(
    frame: &mut Frame,
    r: Rect,
    row: &Row,
    view: Option<&ViewState>,
    parent: Option<&(Derivation, String)>,
    foot: Line<'static>,
) {
    let p = padded(r);
    if p.height == 0 {
        return;
    }
    let width = usize::from(p.width);
    let head = header(row, parent, width);
    frame.render_widget(
        Paragraph::new(head),
        Rect {
            height: PREVIEW_HEADER.min(p.height),
            ..p
        },
    );
    let body = body_area(r);
    let line = |y: u16| Rect { y, height: 1, ..p };
    if p.height > PREVIEW_HEADER {
        frame.render_widget(Paragraph::new(rule(p.width)), line(p.y + PREVIEW_HEADER));
    }
    if body.height == 0 {
        return;
    }
    if let Some(v) = view {
        frame.render_widget(Paragraph::new(transcript(v, body.height)), body);
    }
    frame.render_widget(Paragraph::new(rule(p.width)), line(body.bottom()));
    frame.render_widget(Paragraph::new(foot), line(body.bottom() + 1));
}

/// Title; `agent · id · N msgs · created → modified`; `cwd @ branch`;
/// lineage when derived.
fn header(r: &Row, parent: Option<&(Derivation, String)>, width: usize) -> Vec<Line<'static>> {
    let now = Local::now();
    let id: String = r.session_id.chars().take(8).collect();
    let mut meta = vec![r.agent.as_str().to_string(), id.clone(), msgs(r)];
    meta.push(format!(
        "{} → {}",
        when(&r.created, now),
        when(&r.modified, now)
    ));
    let mut lines = vec![
        Line::styled(
            truncate(&title(r).replace(['\n', '\r', '\t'], " "), width),
            theme::text().add_modifier(Modifier::BOLD),
        ),
        Line::styled(truncate(&meta.join(" · "), width), theme::dim()),
        Line::styled(
            truncate(&format!("{} @ {}", tilde(&r.cwd), branch_or_dash(r)), width),
            theme::dim(),
        ),
    ];
    if let Some((kind, pid)) = parent {
        let how = match kind {
            Derivation::Continued => "rollover",
            Derivation::Subtask => "subtask",
            _ => "trim",
        };
        lines.push(Line::styled(
            truncate(&format!("lineage  {pid} ─{how}─▶ {id}"), width),
            Style::new().fg(theme::LINK),
        ));
    }
    lines
}

/// `match i/n   n/N jump` (or the `/` search) and the scroll gauge.
fn footer(v: &ViewState, full: bool, width: u16) -> Line<'static> {
    let left = if v.typing {
        vec![
            Span::styled("/", theme::heading()),
            Span::styled(v.pattern.clone(), theme::text()),
            Span::styled("▏", theme::heading()),
        ]
    } else {
        let (i, n) = v.match_pos();
        let searching = !v.pattern.is_empty() || !v.keywords.is_empty();
        let mut s = Vec::new();
        if !v.pattern.is_empty() {
            s.push(Span::styled(format!("/{}  ", v.pattern), theme::heading()));
        }
        if searching && n == 0 {
            s.push(Span::styled("no matches", theme::dim()));
        } else if searching {
            s.push(Span::styled(format!("match {i}/{n}   "), theme::dim()));
            s.push(Span::styled("n/N", theme::key()));
            s.push(Span::styled(" jump", theme::dim()));
        } else if full {
            s.push(Span::styled("/", theme::key()));
            s.push(Span::styled(" search", theme::dim()));
        }
        s
    };
    let pct = v.percent();
    let filled = (pct * GAUGE + 50) / 100;
    let gauge = format!(
        "{}{} {pct}%",
        "▮".repeat(filled),
        "▯".repeat(GAUGE - filled)
    );
    let used: usize = left.iter().map(Span::width).sum();
    let gap = usize::from(width).saturating_sub(used + text::width(&gauge));
    let mut out = left;
    if gap > 0 {
        out.push(Span::raw(" ".repeat(gap)));
        out.push(Span::styled(gauge, theme::dim()));
    }
    Line::from(out)
}

/// Ranges of `ranges` starting at the current match's byte in `line`.
fn current_range(
    ranges: &[Range<usize>],
    cur: Option<(usize, usize)>,
    line: usize,
) -> Vec<Range<usize>> {
    cur.filter(|c| c.0 == line)
        .and_then(|c| ranges.iter().find(|r| r.start == c.1).cloned())
        .into_iter()
        .collect()
}

/// The visible transcript rows of `v`.
fn transcript(v: &ViewState, height: u16) -> Vec<Line<'static>> {
    let agent = v.row.agent;
    let pattern = v.pattern_lower();
    let cur = v.current_hit();
    // (line, query ranges, pattern ranges, current) of the line being drawn.
    type Ranges = Vec<Range<usize>>;
    let mut cache: Option<(usize, Ranges, Ranges, Ranges)> = None;
    let mut out = Vec::new();
    for row in v.rows.iter().skip(v.scroll).take(usize::from(height)) {
        let line = &v.lines[row.line];
        if cache.as_ref().is_none_or(|c| c.0 != row.line) {
            let q = keyword_ranges(&line.text, &v.keywords);
            let p = if pattern.is_empty() {
                Vec::new()
            } else {
                find_ci(&line.text, &pattern)
            };
            let c = current_range(if pattern.is_empty() { &q } else { &p }, cur, row.line);
            cache = Some((row.line, q, p, c));
        }
        let Some((_, q, p, c)) = &cache else { continue };
        let (role, base) = match line.kind {
            Kind::User => (Style::new().fg(theme::OK), theme::text()),
            Kind::Assistant => (Style::new().fg(agent_color(agent)), theme::text()),
            Kind::Tool => (theme::dim(), theme::dim()),
            Kind::Plain => (theme::dim(), theme::text()),
        };
        let mut spans_out = Vec::new();
        if line.kind != Kind::Plain {
            let gutter = if line.first && row.first {
                format!("{:>w$} ", label(line.kind, agent), w = GUTTER - 1)
            } else {
                " ".repeat(GUTTER)
            };
            spans_out.push(Span::styled(gutter, role.add_modifier(Modifier::BOLD)));
        }
        spans_out.extend(spans(
            &line.text,
            row.range.clone(),
            base,
            &[
                (q, theme::hit()),
                (p, theme::hit()),
                (c, theme::hit_current()),
            ],
        ));
        out.push(Line::from(spans_out));
    }
    out
}
