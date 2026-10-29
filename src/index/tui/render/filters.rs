//! Filter pane: Agent / Project / When / Kind facets with counts, plus a
//! one-line summary of filters only reachable through `^f`.

use chrono::{Local, NaiveDate};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{bar, fill, inner, pane, selected_bg, tilde};
use crate::index::tui::app::App;
use crate::index::tui::facets::{self, Check, Facet, KindFacet, When};
use crate::index::tui::text::{self, fit, suffix, truncate};
use crate::session::Agent;
use crate::theme;

/// Cells of one "When" radio column (`( ) today  `): the widest row,
/// `( ) today  ( ) yesterday`, fills the pane.
const RADIO_W: usize = 11;

/// One drawn row.
struct Row {
    line: Line<'static>,
    /// Holds the cursor.
    cursor: bool,
    /// Whole-row cursor background (radios highlight only their cell).
    fill: bool,
}

struct Builder<'a> {
    app: &'a App,
    today: NaiveDate,
    focused: bool,
    /// Content width (after the bar column and padding).
    width: usize,
    rows: Vec<Row>,
    /// Index of the next toggleable item.
    item: usize,
}

impl Builder<'_> {
    fn push(&mut self, spans: Vec<Span<'static>>, cursor: bool) {
        let mut v = vec![bar(cursor), Span::raw(" ")];
        v.extend(spans);
        self.rows.push(Row {
            line: Line::from(v),
            cursor,
            fill: cursor,
        });
    }

    fn blank(&mut self) {
        self.rows.push(Row {
            line: Line::default(),
            cursor: false,
            fill: false,
        });
    }

    fn heading(&mut self, title: &str, right: &str) {
        let room = self.width.saturating_sub(text::width(title) + 1);
        let right = fit(&truncate(right, room), room, true);
        self.push(
            vec![
                Span::styled(title.to_string(), theme::heading()),
                Span::raw(" "),
                Span::styled(right, theme::dim()),
            ],
            false,
        );
    }

    /// Advance past one item; whether the cursor is on it.
    fn next_item(&mut self) -> bool {
        let at = self.focused && self.app.facet_cursor == self.item;
        self.item += 1;
        at
    }

    fn check(&mut self, facet: &Facet, label: &str, count: Option<usize>) {
        let cursor = self.next_item();
        let state = facets::state(&self.app.filters, &self.app.projects, facet, self.today);
        let (mark, mark_st, label_st) = match state {
            Check::On => ("[x]", Style::new().fg(theme::OK), theme::text()),
            Check::Partial => ("[-]", Style::new().fg(theme::OK), theme::text()),
            Check::Off => ("[ ]", theme::dim(), theme::dim()),
        };
        let count = count.map(|n| n.to_string()).unwrap_or_default();
        let room = self.width.saturating_sub(4 + text::width(&count) + 1);
        self.push(
            vec![
                Span::styled(mark, mark_st),
                Span::raw(" "),
                Span::styled(fit(label, room, false), label_st),
                Span::raw(" "),
                Span::styled(count, theme::dim()),
            ],
            cursor,
        );
    }

    fn radios(&mut self, pair: [When; 2]) {
        let current = When::of(self.app.filters.after, self.app.filters.before, self.today);
        let mut spans = Vec::new();
        let mut any = false;
        for w in pair {
            let at = self.next_item();
            any |= at;
            let cell = if at { selected_bg() } else { Style::new() };
            let label = format!(" {}", w.label());
            if current == Some(w) {
                spans.extend([
                    Span::styled("(", theme::text().patch(cell)),
                    Span::styled("•", Style::new().fg(theme::OK).patch(cell)),
                    Span::styled(")", theme::text().patch(cell)),
                    Span::styled(label.clone(), theme::text().patch(cell)),
                ]);
            } else {
                spans.push(Span::styled(
                    format!("( ){label}"),
                    theme::dim().patch(cell),
                ));
            }
            spans.push(Span::raw(
                " ".repeat(RADIO_W.saturating_sub(3 + text::width(&label))),
            ));
        }
        let mut v = vec![bar(any), Span::raw(" ")];
        v.extend(spans);
        self.rows.push(Row {
            line: Line::from(v),
            cursor: any,
            fill: false,
        });
    }

    /// Filters set via `^f` / `:` that have no facet.
    fn others(&self) -> Vec<String> {
        let f = &self.app.filters;
        let mut v = Vec::new();
        if let Some(n) = f.min_lines {
            v.push(format!("≥{n}L"));
        }
        let when = When::of(f.after, f.before, self.today);
        if f.after.is_some() && when.is_none() {
            v.extend(f.after.map(|d| format!(">{}", d.display())));
        }
        if when != Some(When::Yesterday) {
            v.extend(f.before.map(|d| format!("<{}", d.display())));
        }
        if f.include_exec {
            v.push("+exec".into());
        }
        if f.live_only {
            v.push("live".into());
        }
        if let (false, Some(b)) = (f.global && f.dir.is_none(), &f.branch) {
            v.push(format!("⎇ {b}"));
        }
        v
    }
}

fn scope_label(app: &App) -> String {
    let f = &app.filters;
    match &f.dir {
        Some(d) => tilde(d),
        None if f.global => "everywhere".into(),
        None => "this dir".into(),
    }
}

/// Draw the filter pane into `r`.
pub fn draw(frame: &mut Frame, app: &App, r: Rect, focused: bool) {
    frame.render_widget(pane(focused), r);
    let area = inner(r);
    let mut b = Builder {
        app,
        today: Local::now().date_naive(),
        focused,
        width: usize::from(area.width).saturating_sub(3),
        rows: Vec::new(),
        item: 0,
    };
    b.push(
        vec![Span::styled(
            "FILTERS",
            theme::dim().add_modifier(Modifier::BOLD),
        )],
        false,
    );
    b.blank();
    b.heading("Agent", "");
    for a in Agent::ALL {
        b.check(&Facet::Agent(a), a.as_str(), Some(app.counts.agent(a)));
    }
    b.blank();
    let scope = scope_label(app);
    let scope = suffix(&scope, b.width.saturating_sub(9)).to_string();
    b.heading("Project", &scope);
    let (shown, more) = facets::shown_projects(&app.counts, &app.projects);
    for (p, n) in shown {
        b.check(&Facet::Project(p.clone()), &p, Some(n));
    }
    if more > 0 {
        // A cursor row: ⏎ opens the project picker.
        let cursor = b.next_item();
        b.push(
            vec![
                Span::styled(format!("…{more} more  "), theme::dim()),
                Span::styled("⏎ all", theme::dim()),
            ],
            cursor,
        );
    }
    b.blank();
    b.heading("When", "");
    b.radios([When::Today, When::Yesterday]);
    b.radios([When::ThreeDays, When::Week]);
    b.radios([When::Month, When::All]);
    b.blank();
    b.heading("Kind", "");
    for k in KindFacet::ALL {
        b.check(&Facet::Kind(k), k.label(), None);
    }
    let others = b.others();
    if !others.is_empty() {
        b.blank();
        b.heading("Other", "^f");
        let s = truncate(&others.join(" · "), b.width);
        b.push(vec![Span::styled(s, theme::dim())], false);
    }
    // Keep the cursor row in view.
    let height = usize::from(area.height);
    let cursor_row = b.rows.iter().position(|r| r.cursor);
    let skip = cursor_row.map_or(0, |c| (c + 1).saturating_sub(height));
    for (i, row) in b.rows.iter().skip(skip).take(height).enumerate() {
        let line_area = Rect {
            y: area.y + i as u16,
            height: 1,
            ..area
        };
        if row.fill {
            fill(frame, line_area, selected_bg());
        }
        frame.render_widget(Paragraph::new(row.line.clone()), line_area);
    }
}
