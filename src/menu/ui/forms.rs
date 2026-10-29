//! Form screens: trim, smart trim, rollover, query and non-launch actions.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::actions::Body;
use super::bold;
use crate::menu::forms::{
    ContinueField, ContinueForm, Input, QUERY_DEFAULT, RolloverType, SMART_TRIM_DEFAULT, TrimField,
    TrimForm,
};
use crate::menu::session::SessionView;
use crate::menu::state::{NonLaunch, NonLaunchKind, QueryForm};
use crate::session::Agent;
use crate::theme;

pub(super) fn trim_form(b: &mut Body, f: &TrimForm) {
    let on = |field| f.field == field;
    b.blank();
    b.field(
        on(TrimField::Tools),
        "Tools to trim",
        &["comma-separated, e.g. bash,read,edit"],
        &f.tools,
        "(all tools)",
    );
    b.blank();
    b.field(
        on(TrimField::Threshold),
        "Length threshold in characters",
        &["tool results longer than this get trimmed"],
        &f.threshold,
        "500",
    );
    b.blank();
    b.field(
        on(TrimField::Assistant),
        "Trim assistant messages (optional)",
        &[
            "Positive (e.g., 10): Trim first N messages exceeding threshold",
            "Negative (e.g., -5): Keep only last N messages",
            "Blank: Skip (no assistant message trimming)",
        ],
        &f.assistant,
        "(skip)",
    );
}

/// Placeholder-until-typed input: (value, placeholder) for [`Body::value`].
fn input_value<'a>(input: &'a Input, default: &'a str, empty: &'a str) -> (&'a str, &'a str) {
    if input.typed {
        (&input.text, empty)
    } else {
        ("", default)
    }
}

pub(super) fn smart_trim(b: &mut Body, input: &Input) {
    let (value, placeholder) = input_value(input, SMART_TRIM_DEFAULT, "(none)");
    b.blank();
    b.field(
        true,
        "Trim instructions",
        &["Tell the agent what to look for when trimming"],
        value,
        placeholder,
    );
}

/// `[1] Claude` (selected, bold FG) or ` 2  Codex` (DIM).
fn choice(selected: bool, n: u8, label: &str) -> Span<'static> {
    if selected {
        Span::styled(format!("[{n}] {label}"), bold(theme::text()))
    } else {
        Span::styled(format!(" {n}  {label}"), theme::dim())
    }
}

pub(super) fn continue_form(b: &mut Body, f: &ContinueForm) {
    let quick = f.rollover_type == RolloverType::Quick;
    b.note(0, "Continue work in a fresh session");
    b.blank();
    b.label(f.field == ContinueField::Agent, "Agent");
    b.push(Line::from(vec![
        Span::raw("  "),
        choice(f.agent == Agent::Claude, 1, "Claude"),
        Span::raw("   "),
        choice(f.agent == Agent::Codex, 2, "Codex"),
    ]));
    b.blank();
    b.label(f.field == ContinueField::RolloverType, "Rollover type");
    b.push(Line::from(vec![
        Span::raw("  "),
        choice(quick, 1, "Quick Resume"),
    ]));
    b.note(
        6,
        "Inject session lineage only, and quickly start new session. \
         You provide context-extraction instructions once session resumes.",
    );
    b.push(Line::from(vec![
        Span::raw("  "),
        choice(!quick, 2, "Resume with Context Recovery"),
    ]));
    b.note(
        6,
        "Inject lineage + use headless/non-interactive agents to extract context. \
         Slower, but resumes with full understanding of last task and overall work.",
    );
    if !quick && f.field == ContinueField::Prompt {
        b.blank();
        b.field(
            true,
            "Custom context recovery instructions (optional)",
            &[],
            &f.prompt,
            "(default: recover context of last task)",
        );
    }
}

pub(super) fn query(b: &mut Body, q: &QueryForm) {
    b.blank();
    if let Some(err) = &q.error {
        b.push(Line::styled(err.clone(), Style::new().fg(theme::WARN)));
        return;
    }
    let (value, placeholder) = input_value(&q.input, QUERY_DEFAULT, "type query...");
    b.field(
        true,
        "Question",
        &["Enter your question about this session (or Enter for default)"],
        value,
        placeholder,
    );
}

pub(super) fn nonlaunch(b: &mut Body, s: &SessionView, nl: &NonLaunch) {
    b.blank();
    if let Some(err) = &nl.error {
        b.push(Line::styled(err.clone(), Style::new().fg(theme::WARN)));
        return;
    }
    match nl.kind {
        NonLaunchKind::Path => {
            b.label(false, "Session file");
            b.value(false, &s.path.display().to_string(), "");
        }
        NonLaunchKind::Copy => b.field(
            true,
            "Destination",
            &["file or directory path"],
            &nl.dest,
            "type path...",
        ),
        NonLaunchKind::Export => {
            b.field(
                true,
                "Export path",
                &["blank = default below; must end in .txt"],
                &nl.dest,
                "type path...",
            );
            b.blank();
            b.label(false, "Default");
            b.value(false, &s.default_export_path.display().to_string(), "");
        }
    }
}
