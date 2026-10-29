//! Drawing, "Inspector" style: an AGENTS pane (filter line + rows), a
//! PREVIEW pane (agent header + live pane capture), a mode footer and a
//! key-help overlay.

use std::borrow::Cow;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, HighlightSpacing, List, ListItem, Padding, Paragraph,
};

use mnemoloom::index::snippet::Snippet;
use mnemoloom::index::tui::render::snippet_spans;
use mnemoloom::index::tui::text::{fit, suffix, width};
use mnemoloom::theme;

use super::app::{App, Mode};
use crate::mux::model::{Agent, Kind, State};
use crate::mux::render;

/// Below this width the preview goes under the list instead of beside it.
const SIDE_BY_SIDE_MIN_WIDTH: u16 = 100;
/// Below this width the preview is hidden until toggled on with `p`.
const PREVIEW_MIN_WIDTH: u16 = 80;
/// Row cells before the name: `● state  KIND `.
const ROW_LEAD: usize = 13;
/// Row cells after the name and repo: ` age`.
const ROW_AGE: usize = 6;
/// Width of the `repo@branch` column.
const REPO: usize = 22;
/// Narrowest name column before the repo column is dropped.
const MIN_NAME: usize = 16;
/// Cells of context kept before a snippet's first matched term.
const SNIPPET_LEAD: usize = 16;

const AGENT_HINTS: &[(&str, &str)] = &[
    ("⏎", "attach"),
    ("/", "filter"),
    ("j/k", "move"),
    ("r", "refresh"),
    ("p", "preview"),
];
const FILTER_HINTS: &[(&str, &str)] = &[
    ("⏎", "attach"),
    ("↑/↓", "move"),
    ("tab", "agents"),
    ("^u", "clear"),
    ("\"…\" !x ~x", "phrase/not/fuzzy"),
];
const HELP: &[(&str, &str)] = &[
    ("⏎", "attach to the highlighted agent's pane"),
    ("/", "filter (typing edits; tab/esc back to the list)"),
    ("j k ↑ ↓", "move (also ^n ^p ^j ^k)"),
    ("g G", "first / last agent"),
    ("PgUp PgDn", "page"),
    ("r ^r", "rescan agents"),
    ("p", "toggle the preview"),
    ("^u ^w ⌫", "clear filter / delete word / char"),
    ("", ""),
    (
        "word",
        "filter: text in name, cwd, repo, branch, model, id…",
    ),
    ("", "  or in the conversation (search index)"),
    ("a b", "all words must match"),
    ("\"a b\"", "exact phrase"),
    ("!word", "exclude agents that contain it"),
    ("~word", "fuzzy match (letters in order)"),
    ("Word", "an uppercase letter makes the word case-sensitive"),
    ("?", "this help (any key closes)"),
    ("q esc ^c", "quit (esc in an empty filter quits too)"),
    ("", ""),
    ("input", "asking you"),
    ("busy", "working"),
    ("bg", "background work running"),
    ("idle", "at the prompt"),
    ("age", "since the last submitted input"),
];

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::new().style(theme::text().bg(theme::BG)), area);
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    app.wide = body.width >= PREVIEW_MIN_WIDTH;
    if app.show_preview() {
        // Stacked, the preview's header and frames need the larger share.
        let (direction, list_share) = if body.width >= SIDE_BY_SIDE_MIN_WIDTH {
            (Direction::Horizontal, 50)
        } else {
            (Direction::Vertical, 40)
        };
        let [list, preview] = Layout::default()
            .direction(direction)
            .constraints([
                Constraint::Percentage(list_share),
                Constraint::Percentage(100 - list_share),
            ])
            .areas(body);
        draw_agents(frame, app, list);
        draw_preview(frame, app, preview);
    } else {
        draw_agents(frame, app, body);
    }
    draw_footer(frame, app, footer);
    if app.help {
        draw_help(frame, area);
    }
}

fn pane(title: &str, focused: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border(focused))
        .title(Span::styled(format!(" {title} "), theme::heading().bold()))
}

/// A one-row separator under a pane's header.
fn rule(area: Rect) -> Paragraph<'static> {
    Paragraph::new("─".repeat(area.width as usize)).style(theme::border(false))
}

fn draw_agents(frame: &mut Frame, app: &mut App, area: Rect) {
    let count = format!(" {}/{} ", app.visible.len(), app.agents.len());
    let block = pane("AGENTS", true)
        .title_top(Line::from(Span::styled(count, theme::dim())).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [filter, sep, list] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    draw_filter(frame, app, filter);
    frame.render_widget(rule(sep), sep);
    draw_list(frame, app, list);
}

/// `/ query▏` on the left; errors, refresh status and state counts on the right.
fn draw_filter(frame: &mut Frame, app: &App, area: Rect) {
    let mut right = Vec::new();
    if let Some(error) = &app.error {
        right.push(Span::styled(
            format!("{error}  "),
            theme::text().fg(theme::WARN),
        ));
    }
    if app.scanning {
        right.push(Span::styled("refreshing…  ", theme::dim()));
    }
    if app.indexing && !app.index_ready {
        right.push(Span::styled("indexing…  ", theme::dim()));
    }
    right.extend(state_counts(&app.agents));
    let right = Line::from(right);
    let [input, status] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(right.width() as u16)])
            .areas(area);
    let slash = Span::styled("/ ", theme::heading().bold());
    let filtering = app.mode == Mode::Filter;
    let query = if app.query.is_empty() {
        Span::styled("filter agents…", theme::dim())
    } else {
        // Keep the tail in view, as an input line does.
        let room = (input.width as usize).saturating_sub(3);
        Span::styled(suffix(&app.query, room).to_owned(), theme::text())
    };
    let cursor = input.x + (2 + width(&app.query)) as u16;
    frame.render_widget(Paragraph::new(Line::from(vec![slash, query])), input);
    frame.render_widget(Paragraph::new(right).right_aligned(), status);
    if filtering && !app.help {
        frame.set_cursor_position((cursor.min(input.right().saturating_sub(1)), input.y));
    }
}

/// `1 busy  2 bg  9 idle`, each in its state colour; absent states omitted.
fn state_counts(agents: &[Agent]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for state in State::ALL {
        let n = agents.iter().filter(|a| a.state == state).count();
        if n > 0 {
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(
                format!("{n} {}", state.as_str()),
                state_style(state),
            ));
        }
    }
    spans
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.visible.is_empty() {
        let note = if app.agents.is_empty() {
            "no agents found"
        } else {
            "no matching agents"
        };
        frame.render_widget(Paragraph::new(note).style(theme::dim()), area);
        return;
    }
    // One cell goes to the selection bar.
    let row_width = (area.width as usize).saturating_sub(1);
    let selected = app.list.selected();
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .enumerate()
        .map(|(n, &i)| {
            let agent = &app.agents[i];
            let mut lines = vec![row_line(agent, row_width)];
            lines.extend(app.hit(agent).map(|h| snippet_line(&h.snippet, row_width)));
            let item = ListItem::new(lines);
            // An item style goes under the spans, so highlighted terms keep
            // their background on the selected row.
            if selected == Some(n) {
                item.style(Style::new().bg(theme::BG_SEL))
            } else {
                item
            }
        })
        .collect();
    let list = List::new(items)
        .highlight_symbol(Span::styled("▌", theme::heading()))
        .highlight_spacing(HighlightSpacing::Always)
        .repeat_highlight_symbol(true);
    frame.render_stateful_widget(list, area, &mut app.list);
}

/// `● state  KIND name  repo@branch  age`, fitted to `cells` cells.
fn row_line(agent: &Agent, cells: usize) -> Line<'static> {
    let style = state_style(agent.state);
    let flexible = cells.saturating_sub(ROW_LEAD + ROW_AGE);
    let repo = match repo_branch(agent) {
        r if r.is_empty() => "—".to_owned(),
        r => r,
    };
    let repo_width = if flexible >= MIN_NAME + 2 + REPO {
        REPO
    } else {
        0
    };
    let name_width = flexible.saturating_sub(if repo_width > 0 { repo_width + 2 } else { 0 });
    let name = if agent.name.is_empty() {
        "-"
    } else {
        &agent.name
    };
    let mut spans = vec![
        Span::styled("● ", style),
        Span::styled(format!("{:<5}  ", agent.state.as_str()), style.bold()),
        badge(agent.kind),
        Span::raw(" "),
        Span::styled(fit(name, name_width, false), theme::key()),
    ];
    if repo_width > 0 {
        spans.push(Span::styled(
            format!("  {}", fit(&repo, repo_width, false)),
            theme::dim(),
        ));
    }
    spans.push(Span::styled(format!(" {:>5}", age(agent)), theme::dim()));
    Line::from(spans)
}

/// `…snippet` of a conversation match under its row, aligned with the name
/// when there is room; matched terms highlighted.
fn snippet_line(snippet: &Snippet, cells: usize) -> Line<'static> {
    let indent = if cells >= ROW_LEAD + MIN_NAME {
        ROW_LEAD
    } else {
        2
    };
    let soft = Style::new().fg(theme::FG_SOFT);
    let mut spans = vec![Span::raw(" ".repeat(indent)), Span::styled("…", soft)];
    let room = cells.saturating_sub(indent + 1);
    if room > 0 {
        let snippet = in_view(snippet, room);
        spans.extend(snippet_spans(&snippet, room, soft, theme::hit()));
    }
    Line::from(spans)
}

/// `snippet`, starting shortly before its first matched term when that
/// term would end past `room` cells.
fn in_view(snippet: &Snippet, room: usize) -> Cow<'_, Snippet> {
    let Some(first) = snippet.highlights.first() else {
        return Cow::Borrowed(snippet);
    };
    if width(&snippet.text[..first.end]) <= room {
        return Cow::Borrowed(snippet);
    }
    let lead = suffix(&snippet.text[..first.start], SNIPPET_LEAD);
    // Start at a word when the lead holds a word break.
    let lead = match lead.find(char::is_whitespace) {
        Some(i) => lead[i..].trim_start(),
        None => lead,
    };
    let start = first.start - lead.len();
    Cow::Owned(Snippet {
        text: snippet.text[start..].to_owned(),
        highlights: snippet
            .highlights
            .iter()
            .map(|r| r.start - start..r.end - start)
            .collect(),
    })
}

/// `repo@branch`, or whichever half is known.
fn repo_branch(agent: &Agent) -> String {
    match (agent.repo.is_empty(), agent.branch.is_empty()) {
        (false, false) => format!("{}@{}", agent.repo, agent.branch),
        (false, true) => agent.repo.clone(),
        (true, false) => format!("@{}", agent.branch),
        (true, true) => String::new(),
    }
}

fn age(agent: &Agent) -> String {
    match agent.input_age_seconds {
        Some(_) => render::input_age(agent),
        None => "–".into(),
    }
}

fn badge(kind: Kind) -> Span<'static> {
    let (label, colour) = match kind {
        Kind::Claude => ("CLD", theme::CLAUDE),
        Kind::Codex => ("CDX", theme::CODEX),
        Kind::Pi => ("PI ", theme::PI),
        Kind::OpenCode => ("OPC", theme::OPENCODE),
    };
    Span::styled(label, Style::new().fg(colour).bold())
}

fn state_style(state: State) -> Style {
    match state {
        State::Input => Style::new().fg(theme::WARN),
        State::Busy => Style::new().fg(theme::ACCENT),
        // Not LINK: that equals CODEX, so `● bg CDX` would read as one block.
        State::Bg => Style::new().fg(theme::OK),
        State::Idle => theme::dim(),
    }
}

fn draw_preview(frame: &mut Frame, app: &App, area: Rect) {
    let block = pane("PREVIEW", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(agent) = app.selected() else {
        frame.render_widget(
            Paragraph::new("no agent selected").style(theme::dim()),
            inner,
        );
        return;
    };
    let header = preview_header(agent);
    let [head, sep, capture] = Layout::vertical([
        Constraint::Length(header.len() as u16),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(header), head);
    frame.render_widget(rule(sep), sep);
    let boxed = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border(false))
        .padding(Padding::horizontal(1));
    let body = boxed.inner(capture);
    frame.render_widget(boxed, capture);
    if app.preview_pane != agent.pane {
        frame.render_widget(Paragraph::new("capturing…").style(theme::dim()), body);
        return;
    }
    // Keep the bottom, as the pane shows it: blank rows under the cursor are
    // dropped and lines are clipped at the box edge, never wrapped.
    let filled = app
        .preview
        .iter()
        .rposition(|line| line.spans.iter().any(|s| !s.content.trim().is_empty()))
        .map_or(0, |last| last + 1);
    let rows = &app.preview[..filled];
    let visible = rows[rows.len().saturating_sub(body.height as usize)..].to_vec();
    frame.render_widget(Paragraph::new(visible), body);
}

/// Title; `pane · kind · model · activity age`; `cwd @ branch`; session id.
/// Parts the scan did not find are left out.
fn preview_header(agent: &Agent) -> Vec<Line<'static>> {
    let title = if agent.name.is_empty() {
        &agent.pane
    } else {
        &agent.name
    };
    let activity = format!("{} {}", agent.inactivity.as_str(), age(agent));
    let about = [
        agent.pane.as_str(),
        agent.kind.as_str(),
        &agent.model,
        &activity,
    ];
    let mut place = tilde(&agent.cwd);
    if !agent.branch.is_empty() {
        place.push_str(&format!(" @ {}", agent.branch));
    }
    let mut lines = vec![
        Line::styled(title.clone(), theme::key()),
        Line::styled(join_present(&about), theme::dim()),
        Line::styled(place, theme::dim()),
    ];
    if !agent.session_id.is_empty() {
        let short: String = agent.session_id.chars().take(8).collect();
        lines.push(Line::styled(
            format!("session {short}"),
            Style::new().fg(theme::LINK),
        ));
    }
    lines
}

fn join_present(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `path` with the home directory shown as `~`.
fn tilde(path: &str) -> String {
    std::env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .and_then(|home| path.strip_prefix(&home).map(|rest| format!("~{rest}")))
        .unwrap_or_else(|| path.to_owned())
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let (chip, hints, right): (&str, _, &[(&str, &str)]) = match app.mode {
        Mode::Agents => (" AGENTS ", AGENT_HINTS, &[("?", "help"), ("q", "quit")]),
        Mode::Filter if app.query.is_empty() => (" FILTER ", FILTER_HINTS, &[("esc", "quit")]),
        Mode::Filter => (" FILTER ", FILTER_HINTS, &[("esc", "agents")]),
    };
    let right = Line::from(hint_spans(right));
    // Drop trailing hints rather than cutting one in half.
    let room = (area.width as usize).saturating_sub(right.width() + 1);
    let mut left = Line::from(Span::styled(chip, theme::chip()));
    for hint in hints.chunks(1) {
        let spans = hint_spans(hint);
        if left.width() + spans.iter().map(Span::width).sum::<usize>() > room {
            break;
        }
        left.spans.extend(spans);
    }
    let [left_area, right_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(right.width() as u16 + 1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(Paragraph::new(right).right_aligned(), right_area);
}

/// `  key label` per hint: bold key, dim label.
fn hint_spans(hints: &[(&'static str, &'static str)]) -> Vec<Span<'static>> {
    hints
        .iter()
        .flat_map(|&(key, label)| {
            [
                Span::raw("  "),
                Span::styled(key, theme::key()),
                Span::styled(format!(" {label}"), theme::dim()),
            ]
        })
        .collect()
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let key_width = HELP.iter().map(|(k, _)| width(k)).max().unwrap_or(0);
    let lines: Vec<Line> = HELP
        .iter()
        .map(|&(key, label)| {
            Line::from(vec![
                Span::styled(fit(key, key_width, false), theme::key()),
                Span::styled(format!("  {label}"), theme::dim()),
            ])
        })
        .collect();
    let text_width = lines.iter().map(Line::width).max().unwrap_or(0);
    let w = (text_width as u16 + 4).min(area.width);
    let h = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let block = pane("KEYS", true).style(theme::text().bg(theme::BG));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(block.padding(Padding::horizontal(1))),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::mux::model::Inactivity;

    fn agent(pane: &str, state: State, kind: Kind, name: &str) -> Agent {
        let mut a = Agent::new(pane.into(), "s".into(), kind);
        a.state = state;
        a.name = name.into();
        a.repo = "webapp".into();
        a.branch = "alice/ABC-123".into();
        a.cwd = "/work/webapp".into();
        a.model = "opus".into();
        a.inactivity = Inactivity::Working;
        a.input_age_seconds = Some(300.0);
        a.session_id = "0123abcd-4567-4e89".into();
        a
    }

    fn app() -> App {
        let mut app = App::new(vec![
            agent("a:1.1", State::Input, Kind::Claude, "Add CLI access"),
            agent("b:1.1", State::Busy, Kind::Codex, "write tests"),
            agent("c:1.1", State::Idle, Kind::Claude, "日本語 docs 😀"),
        ]);
        app.set_preview("a:1.1".into(), "\x1b[32mgreen\x1b[0m output\n$ ");
        app
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn wide_layout_shows_list_preview_and_footer() {
        let mut app = app();
        let screen = render(&mut app, 140, 36);
        assert!(screen.contains("AGENTS"));
        assert!(screen.contains("3/3"));
        assert!(screen.contains("PREVIEW"));
        assert!(screen.contains("▌● input  CLD Add CLI access"));
        assert!(screen.contains("webapp@alice/ABC-123"));
        assert!(screen.contains("a:1.1 · claude · opus · working 5m"));
        assert!(screen.contains("/work/webapp @ alice/ABC-123"));
        assert!(screen.contains("session 0123abcd"));
        assert!(screen.contains("1 input  1 busy  1 idle"));
        assert!(screen.contains("filter agents…"));
        assert!(screen.contains("green output"));
        assert!(screen.contains(" FILTER "));
        assert!(screen.contains("esc quit"));
        press(&mut app, KeyCode::Tab);
        let screen = render(&mut app, 140, 36);
        assert!(screen.contains(" AGENTS "));
        assert!(screen.contains("q quit"));
        press(&mut app, KeyCode::Char('p'));
        assert!(!render(&mut app, 140, 36).contains("PREVIEW"));
    }

    #[test]
    fn narrow_layout_hides_preview_until_toggled() {
        let mut app = app();
        assert!(render(&mut app, 80, 20).contains("PREVIEW"));
        let screen = render(&mut app, 70, 20);
        assert!(!screen.contains("PREVIEW"));
        assert!(screen.contains("CLD Add CLI"));
        press(&mut app, KeyCode::Tab);
        // Footer hints that do not fit are dropped whole.
        let screen = render(&mut app, 70, 20);
        let footer = screen.lines().last().unwrap();
        assert!(footer.contains("r refresh") && !footer.contains("preview"));
        assert!(footer.ends_with("? help  q quit"));
        press(&mut app, KeyCode::Char('p'));
        let screen = render(&mut app, 70, 20);
        assert!(screen.contains("PREVIEW"));
        assert!(screen.contains("green output"));
    }

    #[test]
    fn help_overlay_and_tiny_sizes_do_not_panic() {
        let mut app = app();
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char('?'));
        assert!(render(&mut app, 140, 36).contains("KEYS"));
        for (w, h) in [(1, 1), (5, 3), (20, 4), (69, 8), (100, 10)] {
            render(&mut app, w, h);
        }
        let mut empty = App::new(Vec::new());
        assert!(render(&mut empty, 140, 36).contains("no agents found"));
    }

    /// The preview box's text rows at 140x36, and the box's text width.
    fn preview_rows(app: &mut App) -> (Vec<String>, usize) {
        let mut terminal = Terminal::new(TestBackend::new(140, 36)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        // Preview pane border at x=70, capture box border at 71, padding at 72.
        let (left, cols) = (73, 140 / 2 - 6);
        let rows = (0..36)
            .filter(|&y| buffer[(71, y)].symbol() == "│")
            .map(|y| {
                let row: String = (left..left + cols as u16)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                row.trim_end().to_owned()
            })
            .collect();
        (rows, cols)
    }

    #[test]
    fn preview_clips_long_lines_instead_of_wrapping() {
        let mut app = app();
        let long = format!("{}TAIL", "x".repeat(200));
        app.set_preview("a:1.1".into(), &format!("{long}\nlast"));
        let (rows, cols) = preview_rows(&mut app);
        assert_eq!(rows[0], "x".repeat(cols));
        assert_eq!(rows[1], "last");
        assert!(rows[2..].iter().all(String::is_empty));
    }

    #[test]
    fn preview_is_bottom_anchored_without_trailing_blanks() {
        let mut app = app();
        let text: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        app.set_preview("a:1.1".into(), &format!("{}\n\n  \n\n", text.join("\n")));
        let (rows, _) = preview_rows(&mut app);
        assert_eq!(rows.last().unwrap(), "line 49");
        assert_eq!(rows[0], format!("line {}", 50 - rows.len()));
    }

    #[test]
    fn preview_clips_wide_chars_by_display_width_and_keeps_style() {
        let mut app = app();
        let text = format!("a\x1b[31m{}\x1b[0m", "日".repeat(60));
        app.set_preview("a:1.1".into(), &text);
        let (rows, cols) = preview_rows(&mut app);
        // 64 columns hold `a` and 31 two-column chars; the 32nd would be cut
        // in half, so its last column stays blank. (A wide char's second cell
        // reads back as a space.)
        assert_eq!(cols, 64);
        assert_eq!(rows[0].replace(' ', ""), format!("a{}", "日".repeat(31)));
        assert_eq!(rows[0].chars().count(), 1 + 31 * 2 - 1);
        let mut terminal = Terminal::new(TestBackend::new(140, 36)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let red = (0..36)
            .flat_map(|y| (70..140).map(move |x| (x, y)))
            .find(|&p| buffer[p].symbol() == "日")
            .map(|p| buffer[p].fg);
        assert_eq!(red, Some(Color::Indexed(1)));
    }

    #[test]
    fn pi_and_opencode_badges() {
        let mut app = App::new(vec![
            agent("p:1.1", State::Busy, Kind::Pi, "pi task"),
            agent("o:1.1", State::Idle, Kind::OpenCode, "opencode task"),
        ]);
        let screen = render(&mut app, 140, 36);
        assert!(screen.contains("● busy   PI  pi task"));
        assert!(screen.contains("● idle   OPC opencode task"));
        assert_eq!(badge(Kind::Pi).style.fg, Some(theme::PI));
        assert_eq!(badge(Kind::OpenCode).style.fg, Some(theme::OPENCODE));
        for kind in [Kind::Claude, Kind::Codex, Kind::Pi, Kind::OpenCode] {
            assert_eq!(badge(kind).content.chars().count(), 3);
        }
        assert!(screen.contains("p:1.1 · pi · opus"));
        for c in "opencode".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let screen = render(&mut app, 140, 36);
        assert!(screen.contains("1/2"));
        assert!(screen.contains("OPC opencode task"));
        assert!(!screen.contains("PI  pi task"));
    }

    #[test]
    fn conversation_hit_shows_a_snippet_under_its_row() {
        use std::time::Instant;

        use crate::mux::picker::search::{Hit, Hits};

        let mut app = app();
        app.indexing = true;
        assert!(render(&mut app, 140, 36).contains("indexing…"));
        app.set_index_ready(true);
        assert!(!render(&mut app, 140, 36).contains("indexing…"));
        for c in "rocket".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let later = Instant::now() + crate::mux::picker::app::DEBOUNCE;
        let req = app.search_due(later).unwrap();
        let text = "we moved the rocket launch to friday";
        let start = text.find("rocket").unwrap();
        let snippet = Snippet {
            text: text.into(),
            highlights: std::iter::once(start..start + 6).collect(),
        };
        let hits = Hits::from([(
            "0123abcd-4567-4e89".to_owned(),
            Hit {
                score: 1.0,
                snippet,
            },
        )]);
        assert!(app.set_hits(req.seq, hits));
        let mut terminal = Terminal::new(TestBackend::new(140, 36)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = (0..36)
            .map(|y| (0..70).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        let row = rows
            .iter()
            .position(|r| r.contains("CLD Add CLI access"))
            .unwrap();
        // All three agents share the session id in this fixture.
        assert!(
            rows[row + 1].contains("…we moved the rocket launch to friday"),
            "{}",
            rows[row + 1]
        );
        // Aligned with the name column.
        let col = |line: &str, pat: &str| line[..line.find(pat).unwrap()].chars().count() as u16;
        assert_eq!(col(&rows[row + 1], "…"), col(&rows[row], "Add"));
        let x = col(&rows[row + 1], "rocket");
        assert_eq!(buffer[(x, row as u16 + 1)].style().bg, theme::hit().bg);
        // The rest of the selected card keeps the selection background.
        let before = buffer[(x - 2, row as u16 + 1)].style();
        assert_eq!(
            (before.fg, before.bg),
            (Some(theme::FG_SOFT), Some(theme::BG_SEL))
        );
        assert_eq!(buffer[(x - 2, row as u16 + 1)].fg, theme::FG_SOFT);
    }

    #[test]
    fn snippet_keeps_the_first_match_in_view() {
        let text = format!("{}the needle here and more", "word ".repeat(30));
        let start = text.find("needle").unwrap();
        let s = Snippet {
            text: text.clone(),
            highlights: vec![start..start + 6, start + 7..start + 11],
        };
        let shown = in_view(&s, 40);
        assert_eq!(shown.text, "word word the needle here and more");
        assert_eq!(&shown.text[shown.highlights[0].clone()], "needle");
        assert_eq!(&shown.text[shown.highlights[1].clone()], "here");
        // Already in view, or nothing highlighted: unchanged.
        assert!(matches!(in_view(&s, 200), Cow::Borrowed(_)));
        let plain = Snippet {
            text,
            highlights: Vec::new(),
        };
        assert!(matches!(in_view(&plain, 10), Cow::Borrowed(_)));
    }

    #[test]
    fn bg_state_and_codex_badge_differ() {
        assert_ne!(state_style(State::Bg).fg, Some(theme::CODEX));
    }
}
