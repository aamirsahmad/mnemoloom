//! SESSION pane: header block plus first/last user message.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{bold, dim, heading, pane, short_id, wrapped};
use crate::index::tui::text::truncate;
use crate::menu::session::{PreviewKind, SessionView};
use crate::session::homes::user_home;
use crate::session::{Derivation, session_uuid};
use crate::theme;

pub(super) fn draw_session(frame: &mut Frame, area: Rect, s: &SessionView) {
    let block = pane(Span::styled(" SESSION ", bold(theme::dim())), false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = session_lines(s, usize::from(inner.width));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `~/dev/x` for paths under the user's home.
fn tilde(path: &str) -> String {
    let home = user_home();
    let home = home.to_string_lossy();
    match path.strip_prefix(home.as_ref()) {
        Some(rest) if home.len() > 1 && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_string(),
    }
}

/// `lineage  <parent> ─trim─▶ <this>` for derived sessions.
fn lineage_line(s: &SessionView) -> Option<String> {
    let chain = &s.details.as_ref()?.lineage;
    let (this, parent) = (chain.first()?, chain.get(1)?);
    let kind = match this.derivation {
        Derivation::Trimmed => "trim",
        other => other.friendly(),
    };
    let parent: String = session_uuid(&parent.file).chars().take(8).collect();
    let more = if chain.len() > 2 { "… " } else { "" };
    Some(format!("lineage  {more}{parent} ─{kind}─▶ {}", short_id(s)))
}

/// Header block (as in the search preview) plus first/last user message.
pub fn session_lines(s: &SessionView, width: usize) -> Vec<Line<'static>> {
    let title = s
        .title()
        .or(s.first_title.as_deref())
        .map_or_else(|| short_id(s), str::to_string);
    let mut meta = vec![s.agent.as_str().to_string(), short_id(s)];
    meta.extend(
        [s.lines_label(), s.span_label()]
            .into_iter()
            .filter(|p| !p.is_empty()),
    );
    let place = s.cwd.as_deref().map_or_else(|| s.project.clone(), tilde);
    let place = match &s.branch {
        Some(b) => format!("{place} @ {b}"),
        None => place,
    };
    let mut out = vec![
        Line::styled(truncate(&title, width), bold(theme::text())),
        dim(truncate(&meta.join(" · "), width)),
        dim(truncate(&place, width)),
    ];
    if let Some(l) = lineage_line(s) {
        out.push(Line::styled(
            truncate(&l, width),
            Style::new().fg(theme::LINK),
        ));
    }
    if s.is_sidechain {
        out.push(Line::styled(
            "⚠ Sub-agent session (not resumable)",
            Style::new().fg(theme::WARN),
        ));
    }
    out.push(Line::styled(
        "─".repeat(width),
        Style::new().fg(theme::BORDER),
    ));
    let soft = Style::new().fg(theme::FG_SOFT);
    if let Some(first) = &s.first_message {
        out.push(heading("First user message"));
        out.extend(wrapped(first, width, 4, soft));
        out.push(Line::default());
    }
    out.push(heading(match s.preview_kind {
        PreviewKind::LastUser => "Last user message",
        PreviewKind::LastMessage => "Last message",
    }));
    match &s.details {
        None => out.push(dim("loading…")),
        Some(d) if d.preview.is_empty() => out.push(dim("(none)")),
        Some(d) => out.extend(wrapped(&d.preview, width, 12, soft)),
    }
    out
}
