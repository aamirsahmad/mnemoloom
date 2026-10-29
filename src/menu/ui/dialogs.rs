//! Post-action dialogs: trim result and different-directory confirm.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::actions::{Body, labels};
use super::{bold, dim};
use crate::commands::thousands;
use crate::menu::state::{DIR_ITEMS, DirInfo, TrimInfo};
use crate::theme;

fn check() -> Span<'static> {
    Span::styled("✓ ", Style::new().fg(theme::OK))
}

pub(super) fn trim_confirm(b: &mut Body, info: &TrimInfo, cursor: usize) {
    b.blank();
    if info.nothing_to_trim {
        b.push(Line::from(vec![
            check(),
            Span::styled("Session is already well-optimized", theme::text()),
        ]));
        b.push(dim("  No changes were made"));
    } else {
        let id: String = info
            .new_session_id
            .as_deref()
            .or(info.original_session_id.as_deref())
            .unwrap_or_default()
            .chars()
            .take(12)
            .collect();
        let stat = |label: &str, value: String| {
            Line::from(vec![
                check(),
                Span::styled(format!("{label}: "), theme::dim()),
                Span::styled(value, bold(theme::text())),
            ])
        };
        b.push(stat("New session", format!("{id}...")));
        b.push(stat("Lines trimmed", info.lines_trimmed.to_string()));
        b.push(stat(
            "Tokens saved",
            format!("~{}", thousands(info.tokens_saved as u64)),
        ));
        if !info.output_file.is_empty() {
            b.note(2, &info.output_file);
        }
    }
    b.blank();
    b.items(&labels(&info.items()), cursor);
}

pub(super) fn dir_confirm(b: &mut Body, info: &DirInfo, cursor: usize) {
    b.blank();
    b.note(0, "This session is from a different project directory:");
    b.blank();
    b.label(false, "Current");
    b.value(false, &info.current_dir, "");
    b.label(false, "Session");
    b.value(false, &info.session_dir, "");
    b.blank();
    b.push(Line::styled(
        "Change to the session's directory?",
        theme::text(),
    ));
    b.blank();
    b.items(&labels(&DIR_ITEMS), cursor);
}
