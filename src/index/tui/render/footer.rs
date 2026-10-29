//! One-line footer: an inverse mode chip, key hints, and right-aligned
//! `? help  q quit` (or what Esc does while typing).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::index::tui::app::{App, Focus, InputKind, Mode};
use crate::theme;

type Hints = &'static [(&'static str, &'static str)];

const QUIT: Hints = &[("?", "help"), ("q", "quit")];

fn chip(app: &App) -> &'static str {
    match &app.mode {
        Mode::Normal => match app.focus {
            Focus::Filters => "FILTERS",
            Focus::Results => "RESULTS",
            Focus::Preview => "PREVIEW",
        },
        Mode::Query => "QUERY",
        Mode::Help => "HELP",
        Mode::Command => "CMD",
        Mode::Input(..) => "INPUT",
        Mode::Scope(_) => "SCOPE",
        Mode::Filter(_) => "FILTER",
        Mode::Actions(_) => "ACTIONS",
        Mode::Projects { .. } => "PROJECTS",
        Mode::ConfirmExit | Mode::ConfirmDelete => "CONFIRM",
        Mode::View(_) => "VIEW",
    }
}

/// (left, right) hints for the current mode.
fn hints(app: &App) -> (Hints, Hints) {
    match &app.mode {
        Mode::Normal => match app.focus {
            Focus::Results => (
                &[
                    ("h/l/tab", "pane"),
                    ("j/k", "move"),
                    ("⏎", "resume"),
                    ("a", "actions"),
                    ("y", "copy id"),
                    ("x", "export"),
                    ("t", "trim"),
                ],
                QUIT,
            ),
            Focus::Filters => (
                &[
                    ("h/l", "pane"),
                    ("j/k", "move"),
                    ("space", "toggle"),
                    ("o", "only"),
                    ("s", "scope"),
                    ("^f", "more filters"),
                ],
                QUIT,
            ),
            Focus::Preview => (
                &[
                    ("h/l", "pane"),
                    ("j/k", "scroll"),
                    ("n/N", "match"),
                    ("g/G", "top/end"),
                    ("⏎", "resume"),
                ],
                QUIT,
            ),
        },
        Mode::Query => (
            &[
                ("⏎", "results"),
                ("tab", "filters"),
                ("↑/↓", "move"),
                ("^u", "clear"),
                ("^s", "sort"),
            ],
            &[("esc", "results")],
        ),
        Mode::Help => (&[], &[("esc", "close")]),
        Mode::Command => (
            &[
                ("x", "reset"),
                ("o", "orig"),
                ("s", "sub"),
                ("h", "exec"),
                ("t", "trim"),
                ("c", "roll"),
                ("a", "agent"),
                ("m", "lines"),
                (">", "after"),
                ("<", "before"),
                ("!", "live"),
            ],
            &[("esc", "cancel")],
        ),
        Mode::Input(..) => (&[], &[("⏎", "apply"), ("esc", "cancel")]),
        Mode::Scope(_) => (
            &[("⏎", "choose"), ("j/k", "move"), ("1-3", "pick")],
            &[("esc", "close")],
        ),
        Mode::Filter(_) => (
            &[("⏎", "toggle"), ("j/k", "move"), ("letter", "jump")],
            &[("esc", "close")],
        ),
        Mode::Actions(_) => (
            &[("⏎", "run"), ("j/k", "move"), ("letter", "run")],
            &[("esc", "back")],
        ),
        Mode::Projects { .. } => (
            &[
                ("type", "filter"),
                ("↑/↓", "move"),
                ("space", "toggle"),
                ("⏎", "only this"),
                ("^a", "all"),
            ],
            &[("esc", "close")],
        ),
        Mode::ConfirmExit | Mode::ConfirmDelete => (&[("⏎/y", "yes")], &[("esc/n", "no")]),
        Mode::View(v) if v.typing => (&[("⏎", "search")], &[("esc", "cancel")]),
        Mode::View(_) => (
            &[
                ("j/k", "scroll"),
                ("n/N", "match"),
                ("/", "search"),
                ("g/G", "top/end"),
            ],
            &[("q", "back")],
        ),
    }
}

/// Input-mode prompt (`Min lines: 15▏`).
fn prompt(kind: InputKind, buf: &str) -> String {
    let label = match kind {
        InputKind::Agent => return "agent: 1 claude  2 codex  3 pi  4 opencode  0 all".into(),
        InputKind::MinLines => "min lines",
        InputKind::JumpToRow => "go to row",
        InputKind::AfterDate => "after date",
        InputKind::BeforeDate => "before date",
        InputKind::ScopeDir => "scope dir[:branch]",
    };
    format!("{label}: {buf}▏")
}

fn hint_spans(h: Hints) -> Vec<Vec<Span<'static>>> {
    h.iter()
        .map(|(k, l)| {
            vec![
                Span::styled(*k, theme::key()),
                Span::styled(format!(" {l}"), theme::dim()),
            ]
        })
        .collect()
}

fn width(groups: &[Vec<Span>]) -> usize {
    let w: usize = groups.iter().flatten().map(Span::width).sum();
    w + 2 * groups.len().saturating_sub(1)
}

/// Draw the footer into `r`.
pub fn draw(frame: &mut Frame, app: &App, r: Rect) {
    if r.height == 0 {
        return;
    }
    let total = usize::from(r.width);
    let chip = Span::styled(format!(" {} ", chip(app)), theme::chip());
    let (left, right) = hints(app);
    let right = hint_spans(right);
    let mut left = match (&app.status, &app.mode) {
        (Some(msg), _) => vec![vec![Span::styled(
            format!("✓ {msg}"),
            Style::new().fg(theme::OK),
        )]],
        (None, Mode::Input(kind, buf)) => {
            vec![vec![Span::styled(prompt(*kind, buf), theme::text())]]
        }
        _ => hint_spans(left),
    };
    let fixed = chip.width() + 2;
    let right_w = width(&right);
    // Drop hints from the end until they fit beside the right block.
    while left.len() > 1 && fixed + width(&left) + 2 + right_w > total {
        left.pop();
    }
    let mut spans = vec![chip, Span::raw("  ")];
    for (i, g) in left.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.extend(g);
    }
    let used: usize = spans.iter().map(Span::width).sum();
    if used + 2 + right_w <= total {
        spans.push(Span::raw(" ".repeat(total - used - right_w)));
        for (i, g) in right.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            spans.extend(g);
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), r);
}
