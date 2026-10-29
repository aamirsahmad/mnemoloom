//! Centred overlays: key help, the `^f` filter popup, the scope chooser,
//! the project picker, the action menu and the exit / delete confirmations.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::{Data, bar, fill, short_dir, tilde};
use crate::index::query::Row;
use crate::index::tui::app::{Action, App, FilterItem, Mode};
use crate::index::tui::text::{self, fit};
use crate::session::Agent;
use crate::theme;

/// Draw the overlay for the current mode, if any.
pub fn draw(frame: &mut Frame, app: &App, data: &Data, area: Rect) {
    match &app.mode {
        Mode::Help => help(frame, area),
        Mode::Filter(sel) => filter_modal(frame, app, *sel, area),
        Mode::Scope(sel) => scope_modal(frame, app, *sel, area),
        Mode::Actions(sel) => action_modal(frame, *sel, area),
        Mode::Projects { query, cursor } => projects_modal(frame, app, query, *cursor, area),
        Mode::ConfirmExit => exit_modal(frame, area),
        Mode::ConfirmDelete => {
            if let Some(r) = app.selected_row().map(|i| &data.rows[i]) {
                delete_modal(frame, r, area);
            }
        }
        _ => {}
    }
}

/// A centred rounded box of `w` x `h` (clamped); returns its padded inside.
fn modal(frame: &mut Frame, area: Rect, w: u16, h: u16, title: &str) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    frame.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border(true))
        .title(Span::styled(
            format!(" {title} "),
            theme::heading().add_modifier(Modifier::BOLD),
        ))
        .style(theme::text().bg(theme::BG));
    let inner = block.inner(r);
    frame.render_widget(block, r);
    inner.inner(ratatui::layout::Margin::new(1, 0))
}

/// A list row: ACCENT `▌` + BG_SEL when selected.
fn menu_line(selected: bool, label: String, extra: Vec<Span<'static>>) -> Line<'static> {
    let st = if selected {
        theme::text().bg(theme::BG_SEL)
    } else {
        theme::text()
    };
    let mut spans = vec![
        bar(selected),
        Span::styled(" ", st),
        Span::styled(label, st),
    ];
    spans.extend(extra);
    Line::from(spans)
}

const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Panes",
        &[
            ("h/l  tab/S-tab", "move pane focus"),
            ("j/k  ↑/↓  PgUp/PgDn", "move in the focused pane"),
            ("g/G  Home/End", "first / last"),
            ("f / p", "show or hide filters / preview"),
            ("esc", "back to results; clear the query; quit"),
            ("q  ^c", "quit"),
        ],
    ),
    (
        "Search",
        &[
            ("/", "edit the query (⏎/esc: back to results)"),
            ("other keys", "in results: type into the query"),
            ("^u", "clear the query (while editing)"),
            ("^s", "sort by relevance / time"),
            ("s", "scope: everywhere / this dir / dir[:branch]"),
            ("P", "projects: pick from all of them"),
            ("space  ⏎", "toggle the facet under the cursor"),
            ("^f", "all filters (exec runs, live, lines, dates)"),
            (":", "filter command (one key)"),
            ("^g", "go to result number"),
        ],
    ),
    (
        "Session",
        &[
            ("⏎", "resume"),
            ("a", "actions menu (view, copy, query, …)"),
            ("y", "copy session id"),
            ("x", "export to text"),
            ("t", "trim + resume"),
            ("n/N", "next / previous match in the preview"),
        ],
    ),
];

fn help(frame: &mut Frame, area: Rect) {
    let rows: usize = HELP.iter().map(|(_, ks)| ks.len() + 2).sum();
    let inner = modal(frame, area, 70, rows as u16 + 1, "Keys");
    let mut lines = Vec::new();
    for (i, (title, keys)) in HELP.iter().enumerate() {
        if i > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::styled(*title, theme::heading()));
        for (k, what) in *keys {
            lines.push(Line::from(vec![
                Span::styled(format!("{k:>20}"), theme::key()),
                Span::raw("  "),
                Span::styled(*what, theme::dim()),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn filter_modal(frame: &mut Frame, app: &App, sel: usize, area: Rect) {
    let inner = modal(
        frame,
        area,
        44,
        FilterItem::ALL.len() as u16 + 2,
        "Filters (^f)",
    );
    let f = &app.filters;
    let on_off = |b: bool| if b { " [ON]" } else { " [off]" }.to_string();
    let dot = |b: bool| if b { " ●" } else { " ○" }.to_string();
    let lines: Vec<Line> = FilterItem::ALL
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let state = match item {
                FilterItem::Reset => String::new(),
                FilterItem::Original => on_off(f.include_original),
                FilterItem::Sub => on_off(f.include_sub),
                FilterItem::Exec => on_off(f.include_exec),
                FilterItem::Trimmed => on_off(f.include_trimmed),
                FilterItem::Rollover => on_off(f.include_continued),
                FilterItem::Live => on_off(f.live_only),
                FilterItem::AllAgents => dot(f.agents.is_all()),
                FilterItem::ClaudeOnly => dot(f.agents.single() == Some(Agent::Claude)),
                FilterItem::CodexOnly => dot(f.agents.single() == Some(Agent::Codex)),
                FilterItem::PiOnly => dot(f.agents.single() == Some(Agent::Pi)),
                FilterItem::OpenCodeOnly => dot(f.agents.single() == Some(Agent::OpenCode)),
                FilterItem::MinLines => f.min_lines.map_or(" [Any]".into(), |n| format!(" [≥{n}]")),
                FilterItem::After => f
                    .after
                    .map_or(" [None]".into(), |d| format!(" [>{}]", d.display())),
                FilterItem::Before => f
                    .before
                    .map_or(" [None]".into(), |d| format!(" [<{}]", d.display())),
            };
            menu_line(
                i == sel,
                item.label().to_string(),
                vec![Span::styled(state, Style::new().fg(theme::OK))],
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn scope_modal(frame: &mut Frame, app: &App, sel: usize, area: Rect) {
    let inner = modal(frame, area, 80, 5, "Scope (s)");
    let f = &app.filters;
    let mut current = format!("Current directory ({})", short_dir(&f.launch_cwd, 50));
    if let Some(b) = &f.branch {
        current.push_str(&format!(" [⎇ {b}]"));
    }
    let custom = match (&f.dir, &f.branch) {
        (Some(d), Some(b)) => format!(" [{}:{b}]", tilde(d)),
        (Some(d), None) => format!(" [{}]", tilde(d)),
        (None, Some(b)) => format!(" [:{b}]"),
        (None, None) => String::new(),
    };
    let custom = if custom.chars().count() > 40 {
        format!("{}...]", custom.chars().take(37).collect::<String>())
    } else {
        custom
    };
    let items = [
        (
            "Global (everywhere)".to_string(),
            f.global && f.dir.is_none(),
            String::new(),
        ),
        (current, !f.global && f.dir.is_none(), String::new()),
        (
            "Custom directory/branch...".to_string(),
            f.dir.is_some(),
            custom,
        ),
    ];
    let lines: Vec<Line> = items
        .into_iter()
        .enumerate()
        .map(|(i, (label, active, suffix))| {
            menu_line(
                i == sel,
                label,
                vec![
                    Span::styled(if active { " ●" } else { " ○" }, Style::new().fg(theme::OK)),
                    Span::styled(suffix, theme::dim()),
                ],
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Rows of the project picker overlay (besides the list).
const PICKER_CHROME: u16 = 4;

fn projects_modal(frame: &mut Frame, app: &App, query: &str, cursor: usize, area: Rect) {
    let items = app.picker_items(query);
    let h = (items.len() as u16 + PICKER_CHROME).clamp(8, 28);
    let inner = modal(frame, area, 60, h, "PROJECTS");
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let width = usize::from(inner.width);
    let prompt = format!("/ {query}▏");
    let n = match items.len() {
        1 => "1 project".to_string(),
        n => format!("{n} projects"),
    };
    let room = width.saturating_sub(text::width(&prompt) + 1);
    let top = Line::from(vec![
        Span::styled(prompt, theme::text()),
        Span::raw(" "),
        Span::styled(fit(&n, room, true), theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(top), Rect { height: 1, ..inner });
    let list = Rect {
        y: inner.y + 2,
        height: inner.height.saturating_sub(2),
        ..inner
    };
    let rows = usize::from(list.height);
    let cursor = cursor.min(items.len().saturating_sub(1));
    let skip = (cursor + 1).saturating_sub(rows);
    for (i, (p, count)) in items.iter().enumerate().skip(skip).take(rows) {
        let selected = i == cursor;
        let (mark, mark_st, label_st) = if app.projects.shows(p) {
            ("[x]", Style::new().fg(theme::OK), theme::text())
        } else {
            ("[ ]", theme::dim(), theme::dim())
        };
        let count = count.to_string();
        let room = width.saturating_sub(6 + text::width(&count) + 1);
        let line_area = Rect {
            y: list.y + (i - skip) as u16,
            height: 1,
            ..list
        };
        if selected {
            fill(frame, line_area, Style::new().bg(theme::BG_SEL));
        }
        let line = Line::from(vec![
            bar(selected),
            Span::raw(" "),
            Span::styled(mark, mark_st),
            Span::raw(" "),
            Span::styled(fit(p, room, false), label_st),
            Span::raw(" "),
            Span::styled(count, theme::dim()),
        ]);
        frame.render_widget(Paragraph::new(line), line_area);
    }
}

fn action_modal(frame: &mut Frame, sel: usize, area: Rect) {
    let inner = modal(
        frame,
        area,
        56,
        Action::ALL.len() as u16 + 2,
        "Session actions",
    );
    let lines: Vec<Line> = Action::ALL
        .iter()
        .enumerate()
        .map(|(i, a)| menu_line(i == sel, a.label().to_string(), Vec::new()))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn key_line(keys: &[(&'static str, &'static str)]) -> Line<'static> {
    let mut v = Vec::new();
    for (k, what) in keys {
        v.push(Span::styled(*k, theme::key()));
        v.push(Span::styled(format!(" {what}  "), theme::dim()));
    }
    Line::from(v)
}

fn exit_modal(frame: &mut Frame, area: Rect) {
    let inner = modal(frame, area, 52, 6, "Exit?");
    let lines = vec![
        Line::styled("You have active filters set.", theme::text()),
        Line::default(),
        key_line(&[("⏎", "exit and lose filter settings")]),
        key_line(&[("esc", "cancel and return to search")]),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn delete_modal(frame: &mut Frame, r: &Row, area: Rect) {
    let inner = modal(frame, area, 60, 8, "Delete session?");
    let id: String = r.session_id.chars().take(12).collect();
    let branch = if r.branch.is_empty() {
        "—"
    } else {
        &r.branch
    };
    let lines = vec![
        Line::styled(
            format!("Session: {id}...  Lines: {}", r.lines),
            theme::text(),
        ),
        Line::styled(
            format!("Project: {}  Branch: {branch}", r.project_name()),
            theme::text(),
        ),
        Line::default(),
        Line::styled(
            "This action cannot be undone!",
            Style::new().fg(theme::WARN).add_modifier(Modifier::BOLD),
        ),
        Line::default(),
        key_line(&[("⏎", "confirm delete"), ("esc", "cancel")]),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}
