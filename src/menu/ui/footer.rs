//! Footer (mode chip + key hints) and the `?` help overlay.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use super::{bold, pane};
use crate::index::tui::text::{fit, width as display_width};
use crate::menu::state::{NonLaunchKind, Screen, State};
use crate::theme;

/// Mode chip, left key hints and right-aligned hints for the screen.
struct Keys {
    chip: &'static str,
    hints: Vec<(&'static str, &'static str)>,
    right: Vec<(&'static str, &'static str)>,
}

const LIST: [(&str, &str); 3] = [("⏎", "select"), ("j/k", "move"), ("1-9", "jump")];
const HELP: (&str, &str) = ("?", "help");
const BACK: (&str, &str) = ("esc", "back");

fn keys(state: &State) -> Keys {
    let list = |chip, esc| Keys {
        chip,
        hints: LIST.into_iter().chain([esc]).collect(),
        right: vec![HELP],
    };
    let text = |chip, hints: &[(&'static str, &'static str)]| Keys {
        chip,
        hints: hints.to_vec(),
        right: vec![BACK],
    };
    let view = |chip, hints: &[(&'static str, &'static str)]| Keys {
        chip,
        hints: hints.to_vec(),
        right: vec![HELP],
    };
    match &state.screen {
        Screen::Results => list("SESSIONS", ("esc", "quit")),
        Screen::Action | Screen::Resume | Screen::TrimMenu => list("ACTIONS", BACK),
        Screen::Trim(_) => text("TRIM", &[("⏎", "next/submit"), ("↑/↓", "field")]),
        Screen::SmartTrim(_) => text("SMART TRIM", &[("⏎", "submit")]),
        Screen::Lineage => view("LINEAGE", &[("⏎", "options"), BACK]),
        Screen::Continue(_) => text(
            "ROLLOVER",
            &[("⏎", "next/submit"), ("↑/↓", "field"), ("1/2", "choose")],
        ),
        Screen::Query(q) if q.error.is_some() => view("QUERY", &[("⏎/esc", "back")]),
        Screen::Query(_) => text("QUERY", &[("⏎", "run query")]),
        Screen::NonLaunch(nl) if nl.error.is_some() => {
            view(nl.kind.badge(), &[("⏎", "exit"), BACK])
        }
        Screen::NonLaunch(nl) if nl.kind == NonLaunchKind::Path => {
            view("PATH", &[("⏎", "print & exit"), BACK])
        }
        Screen::NonLaunch(nl) => text(nl.kind.badge(), &[("⏎", "run")]),
        Screen::TrimConfirm(info) => list(
            "TRIM",
            if info.nothing_to_trim {
                BACK
            } else {
                ("esc", "cancel (keep file)")
            },
        ),
        Screen::DirConfirm(_) => list("CONFIRM", ("esc", "cancel")),
    }
}

fn hint_spans(hints: &[(&'static str, &'static str)]) -> Vec<Span<'static>> {
    hints
        .iter()
        .flat_map(|(k, label)| {
            [
                Span::raw("  "),
                Span::styled(*k, theme::key()),
                Span::styled(format!(" {label}"), theme::dim()),
            ]
        })
        .collect()
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// ` ACTIONS   ⏎ select  j/k move ...            ? help` at `width`.
pub fn footer_line(state: &State, width: u16) -> Line<'static> {
    let k = if state.help {
        Keys {
            chip: "HELP",
            hints: vec![("any key", "close")],
            right: Vec::new(),
        }
    } else {
        keys(state)
    };
    let width = usize::from(width);
    let mut spans = vec![Span::styled(format!(" {} ", k.chip), theme::chip())];
    let right = hint_spans(&k.right);
    let right_w = spans_width(&right);
    for hint in &k.hints {
        let next = hint_spans(std::slice::from_ref(hint));
        if spans_width(&spans) + spans_width(&next) + right_w > width {
            break;
        }
        spans.extend(next);
    }
    let used = spans_width(&spans);
    if used + right_w <= width {
        spans.push(Span::raw(" ".repeat(width - used - right_w)));
        spans.extend(right);
    }
    Line::from(spans)
}

/// Every key of the current screen, for the overlay.
fn help_rows(state: &State) -> Vec<(&'static str, &'static str)> {
    let k = keys(state);
    let mut rows = k.hints;
    rows.extend(k.right.into_iter().filter(|r| *r != HELP));
    if !state.text_entry() {
        rows.push(("q", "same as esc"));
        rows.push(HELP);
    }
    rows.push(("^c", "quit now"));
    rows
}

pub(super) fn draw_help(frame: &mut Frame, area: Rect, state: &State) {
    let rows = help_rows(state);
    let key_w = rows
        .iter()
        .map(|(k, _)| display_width(k))
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, label)| {
            Line::from(vec![
                Span::styled(fit(k, key_w, true), theme::key()),
                Span::styled(format!("  {label}"), theme::dim()),
            ])
        })
        .collect();
    let w = (lines.iter().map(Line::width).max().unwrap_or(0) + 4).max(16);
    let h = lines.len() + 2;
    let w = u16::try_from(w).unwrap_or(u16::MAX).min(area.width);
    let h = u16::try_from(h).unwrap_or(u16::MAX).min(area.height);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    let block = pane(Span::styled(" KEYS ", bold(theme::heading())), true)
        .style(Style::new().bg(theme::BG));
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}
