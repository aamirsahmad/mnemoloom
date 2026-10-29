//! ACTIONS pane: the [`Body`] builder, screen dispatch, lineage and the
//! results picker.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::dialogs::{dir_confirm, trim_confirm};
use super::forms::{continue_form, nonlaunch, query, smart_trim, trim_form};
use super::{BAR, Ctx, agent_badge, bold, dim, heading, pane, short_id};
use crate::index::tui::text::{fit, width as display_width, wrap_text};
use crate::menu::session::SessionView;
use crate::menu::state::{Screen, State, TRIM_SUBMENU, actions, resume_items};
use crate::theme;

/// Lines of the right-hand pane with the line to keep in view.
#[derive(Default)]
pub(super) struct Body {
    lines: Vec<Line<'static>>,
    focus: usize,
    width: usize,
}

impl Body {
    fn new(width: usize) -> Body {
        Body {
            width,
            ..Body::default()
        }
    }

    pub(super) fn push(&mut self, line: Line<'static>) {
        self.lines.push(line);
    }

    pub(super) fn blank(&mut self) {
        self.lines.push(Line::default());
    }

    fn focus_here(&mut self) {
        self.focus = self.lines.len();
    }

    /// DIM text wrapped under an `indent`.
    pub(super) fn note(&mut self, indent: usize, text: &str) {
        let pad = " ".repeat(indent);
        for l in wrap_text(text, self.width.saturating_sub(indent).max(1)) {
            self.push(dim(format!("{pad}{l}")));
        }
    }

    /// Field label: ACCENT with a bar when focused, DIM otherwise.
    pub(super) fn label(&mut self, focused: bool, text: &str) {
        if focused {
            self.focus_here();
            self.push(Line::from(vec![
                Span::styled(BAR, theme::heading()),
                Span::styled(format!(" {text}"), theme::heading()),
            ]));
        } else {
            self.push(dim(format!("  {text}")));
        }
    }

    /// Value under a label: `› ` then FG text (or a DIM placeholder) and,
    /// when focused, a block cursor.
    pub(super) fn value(&mut self, focused: bool, value: &str, placeholder: &str) {
        let (text, style) = if value.is_empty() {
            (placeholder, theme::dim())
        } else {
            (value, theme::text())
        };
        let mut rows = wrap_text(text, self.width.saturating_sub(5).max(1));
        if rows.is_empty() {
            rows.push(String::new());
        }
        let last = rows.len() - 1;
        let mark = if focused {
            theme::heading()
        } else {
            theme::dim()
        };
        for (i, row) in rows.into_iter().enumerate() {
            let lead = if i == 0 { "  › " } else { "    " };
            let mut spans = vec![Span::styled(lead, mark), Span::styled(row, style)];
            if focused && i == last {
                spans.push(Span::styled(" ", Style::new().bg(theme::FG)));
            }
            self.push(Line::from(spans));
        }
    }

    /// Label, wrapped hints and value.
    pub(super) fn field(
        &mut self,
        focused: bool,
        label: &str,
        hints: &[&str],
        value: &str,
        placeholder: &str,
    ) {
        self.label(focused, label);
        for h in hints {
            self.note(2, h);
        }
        self.value(focused, value, placeholder);
    }

    /// Numbered rows `1  Label`; the selected one gets the bar + BG_SEL.
    pub(super) fn items(&mut self, labels: &[&str], cursor: usize) {
        for (i, label) in labels.iter().enumerate() {
            let selected = i == cursor;
            if selected {
                self.focus_here();
            }
            let num = format!(" {}  ", i + 1);
            let rest = self.width.saturating_sub(1 + display_width(&num));
            let label = fit(label, rest, false);
            self.push(if selected {
                let sel = Style::new().bg(theme::BG_SEL);
                Line::from(vec![
                    Span::styled(BAR, theme::heading().bg(theme::BG_SEL)),
                    Span::styled(num, theme::heading().bg(theme::BG_SEL)),
                    Span::styled(label, bold(theme::text()).patch(sel)),
                ])
            } else {
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(num, theme::dim()),
                    Span::styled(label, theme::text()),
                ])
            });
        }
    }
}

pub(super) fn labels<T>(items: &[(T, &'static str)]) -> Vec<&'static str> {
    items.iter().map(|(_, l)| *l).collect()
}

pub(super) fn draw_main(
    frame: &mut Frame,
    area: Rect,
    state: &State,
    ctx: &Ctx,
    compact: Option<&SessionView>,
) {
    let (title, body) = main_body(state, ctx, usize::from(area.width.saturating_sub(4)));
    let mut spans = vec![Span::styled(format!(" {title} "), bold(theme::heading()))];
    if let Some(s) = compact {
        spans.extend([
            agent_badge(s.agent),
            Span::styled(format!(" {} ", short_id(s)), theme::dim()),
        ]);
    }
    let block = pane(Line::from(spans), true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let height = usize::from(inner.height);
    let max = body.lines.len().saturating_sub(height);
    let offset = (body.focus + 2).saturating_sub(height).min(max);
    let offset = u16::try_from(offset).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(body.lines).scroll((offset, 0)), inner);
}

/// Pane title and body of the current screen at `width` inner cells.
fn main_body(state: &State, ctx: &Ctx, width: usize) -> (String, Body) {
    let mut b = Body::new(width);
    let title = match &state.screen {
        Screen::TrimConfirm(info) => {
            trim_confirm(&mut b, info, state.cursor);
            if info.nothing_to_trim {
                "NOTHING TO TRIM"
            } else {
                "TRIM COMPLETE"
            }
            .to_string()
        }
        Screen::DirConfirm(info) => {
            dir_confirm(&mut b, info, state.cursor);
            "DIFFERENT DIRECTORY".into()
        }
        Screen::Results => {
            results(&mut b, ctx, state.cursor);
            ctx.results_title.trim().to_uppercase()
        }
        screen => {
            let Some(s) = ctx.sessions.get(state.current) else {
                return (String::new(), b);
            };
            session_screen(&mut b, screen, s, state.cursor)
        }
    };
    (title, b)
}

fn session_screen(b: &mut Body, screen: &Screen, s: &SessionView, cursor: usize) -> String {
    match screen {
        Screen::Action => {
            b.blank();
            b.items(&labels(actions(s)), cursor);
            "ACTIONS"
        }
        Screen::Resume => {
            b.blank();
            b.items(&labels(resume_items(s.agent)), cursor);
            "RESUME / TRIM"
        }
        Screen::TrimMenu => {
            b.blank();
            b.items(&labels(&TRIM_SUBMENU), cursor);
            "TRIM SESSION"
        }
        Screen::Trim(form) => {
            trim_form(b, form);
            "TRIM OPTIONS"
        }
        Screen::SmartTrim(input) => {
            smart_trim(b, input);
            "SMART TRIM"
        }
        Screen::Lineage => {
            lineage(b, s);
            "LINEAGE"
        }
        Screen::Continue(form) => {
            continue_form(b, form);
            "ROLLOVER"
        }
        Screen::Query(q) => {
            query(b, q);
            "QUERY"
        }
        Screen::NonLaunch(nl) => {
            nonlaunch(b, s, nl);
            nl.kind.badge()
        }
        Screen::Results | Screen::TrimConfirm(_) | Screen::DirConfirm(_) => "",
    }
    .to_string()
}

fn lineage(b: &mut Body, s: &SessionView) {
    b.blank();
    match &s.details {
        None => b.push(dim("Loading session lineage...")),
        Some(d) if d.lineage.is_empty() => {
            b.note(0, "This is the original session (no continuation history)");
        }
        Some(d) => {
            b.push(heading(&format!(
                "Found {} session(s) in continuation chain:",
                d.lineage.len()
            )));
            b.blank();
            for (i, n) in d.lineage.iter().enumerate() {
                b.push(Line::from(vec![
                    Span::styled(format!(" {}  ", i + 1), theme::dim()),
                    Span::styled(n.file.clone(), theme::text()),
                    Span::styled(format!(" ({})", n.derivation.as_str()), theme::dim()),
                ]));
            }
        }
    }
}

/// Candidate cards: `▌ 1 CLD title  date` / `project @ branch · N user msgs`.
fn results(b: &mut Body, ctx: &Ctx, cursor: usize) {
    if let Some(scope) = ctx.scope_line {
        b.note(0, scope);
    }
    b.blank();
    for (i, s) in ctx.sessions.iter().enumerate() {
        let selected = i == cursor;
        if selected {
            b.focus_here();
        }
        let bg = if selected {
            Style::new().bg(theme::BG_SEL)
        } else {
            Style::new()
        };
        let bar = Span::styled(if selected { BAR } else { " " }, theme::heading());
        let num = format!(" {}  ", i + 1);
        let date = s.date_range();
        let title = s
            .title()
            .or(s.first_title.as_deref())
            .map_or_else(|| short_id(s), str::to_string);
        let room = b
            .width
            .saturating_sub(1 + display_width(&num) + 4 + display_width(&date) + 1);
        let top = vec![
            bar.clone(),
            Span::styled(num.clone(), theme::dim()),
            agent_badge(s.agent),
            Span::raw(" "),
            Span::styled(fit(&title, room, false), bold(theme::text())),
            Span::raw(" "),
            Span::styled(date, theme::dim()),
        ];
        let place = match &s.branch {
            Some(br) => format!("{} @ {br}", s.project),
            None => s.project.clone(),
        };
        let mut meta = vec![place];
        meta.extend(
            [s.lines_label(), s.annotation()]
                .into_iter()
                .filter(|p| !p.is_empty()),
        );
        let pad = " ".repeat(display_width(&num) + 4);
        let rest = b.width.saturating_sub(1 + pad.len());
        let bottom = vec![
            bar,
            Span::styled(
                format!("{pad}{}", fit(&meta.join(" · "), rest, false)),
                theme::dim(),
            ),
        ];
        for spans in [top, bottom] {
            b.push(Line::from(
                spans
                    .into_iter()
                    .map(|sp| sp.patch_style(bg))
                    .collect::<Vec<_>>(),
            ));
        }
    }
    let legend = legend(ctx.sessions);
    if !legend.is_empty() {
        b.blank();
        b.note(0, &legend);
    }
}

/// `(t)=trimmed (r)=rolled-over (sub)=sub-agent (not resumable)`, as present.
fn legend(sessions: &[SessionView]) -> String {
    let annos: Vec<String> = sessions.iter().map(SessionView::annotation).collect();
    let has = |tag: &str| {
        annos.iter().any(|a| {
            a.trim_matches(['(', ')'])
                .split(',')
                .any(|part| part == tag)
        })
    };
    let mut parts = Vec::new();
    if has("t") {
        parts.push("(t)=trimmed");
    }
    if has("r") {
        parts.push("(r)=rolled-over");
    }
    if has("sub") {
        parts.push("(sub)=sub-agent (not resumable)");
    }
    parts.join(" ")
}
