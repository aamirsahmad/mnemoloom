//! What the menu shows about a session: header fields read up front, plus
//! [`Details`] (user-message count, preview, lineage) loaded in the
//! background so the first frame does not wait for a full file scan.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Local, NaiveDate};

use crate::session::discover::codex_thread_names;
use crate::session::lineage::full_chain;
use crate::session::message::{MessageKind, Role, display_title, preview_text, strip_paste_tags};
use crate::session::meta::count_user_messages;
use crate::session::timefmt::parse_iso;
use crate::session::{Agent, Depth, Derivation, Homes, Resolution, SessionFile, Transcript};

/// Timestamps before 2000-01-01 are treated as missing.
const MIN_VALID_TS: i64 = 946_684_800;

/// Which message the preview shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    /// Last real user message (session given explicitly).
    LastUser,
    /// `[role] <last message>` (latest-session candidates).
    LastMessage,
}

/// One lineage row: file name and how it was derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageNode {
    pub file: String,
    pub derivation: Derivation,
}

/// Fields that need a full read of the transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Details {
    pub user_messages: usize,
    pub preview: String,
    /// Custom title (Claude content name / Codex thread name).
    pub title: Option<String>,
    /// Empty for an original session.
    pub lineage: Vec<LineageNode>,
}

/// A session as shown and acted on by the menu.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionView {
    pub agent: Agent,
    pub session_id: String,
    pub path: PathBuf,
    pub cwd: Option<String>,
    pub project: String,
    pub branch: Option<String>,
    pub is_sidechain: bool,
    pub derivation: Derivation,
    /// Unix seconds; `None` when unknown.
    pub created: Option<i64>,
    pub modified: Option<i64>,
    /// First real user message, paste tags stripped (header read, ≤200 chars).
    pub first_message: Option<String>,
    /// One-line title from the first user message ([`display_title`]).
    pub first_title: Option<String>,
    pub default_export_path: PathBuf,
    pub preview_kind: PreviewKind,
    pub details: Option<Details>,
}

impl SessionView {
    /// Build from header metadata (cheap); `details` stays `None`.
    pub fn from_file(sf: &SessionFile, preview_kind: PreviewKind) -> SessionView {
        let created = sf
            .created
            .as_deref()
            .and_then(parse_iso)
            .map(|d| d.timestamp())
            .or_else(|| unix(sf.changed));
        SessionView {
            agent: sf.agent,
            session_id: sf.session_id.clone(),
            path: sf.path.clone(),
            cwd: sf.cwd.clone(),
            project: sf.project(),
            branch: sf.git_branch.clone().filter(|b| !b.is_empty()),
            is_sidechain: sf.is_sidechain,
            derivation: sf.derivation(),
            created,
            modified: unix(sf.modified),
            first_message: sf
                .first_user_message
                .as_deref()
                .map(|m| preview_text(&strip_paste_tags(m)))
                .filter(|m| !m.is_empty()),
            first_title: sf
                .first_user_message
                .as_deref()
                .map(display_title)
                .filter(|m| !m.is_empty()),
            default_export_path: sf.default_export_path(),
            preview_kind,
            details: None,
        }
    }

    /// Build from a resolved session (explicit SESSION argument).
    pub fn from_resolution(res: &Resolution) -> anyhow::Result<SessionView> {
        let mut sf = res.metadata(Depth::Header)?;
        if sf.cwd.is_none() {
            sf.cwd = res.cwd.clone();
        }
        if sf.git_branch.is_none() {
            sf.git_branch = res.git_branch.clone();
        }
        sf.path = res.session_file.clone();
        Ok(SessionView::from_file(&sf, PreviewKind::LastUser))
    }

    /// Custom title, once details are loaded.
    pub fn title(&self) -> Option<&str> {
        self.details.as_ref()?.title.as_deref()
    }

    /// `<n> user msgs` (`1 user msg`), or empty while loading.
    pub fn lines_label(&self) -> String {
        self.details
            .as_ref()
            .map(|d| match d.user_messages {
                1 => "1 user msg".to_string(),
                n => format!("{n} user msgs"),
            })
            .unwrap_or_default()
    }

    /// `(r,t,sub)`-style annotation, or empty.
    pub fn annotation(&self) -> String {
        let mut parts = Vec::new();
        match self.derivation {
            Derivation::Continued => parts.push("r"),
            Derivation::Trimmed => parts.push("t"),
            // opencode child sessions also carry `is_sidechain` ("sub").
            Derivation::Original | Derivation::Subtask => {}
        }
        if self.is_sidechain {
            parts.push("sub");
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("({})", parts.join(","))
        }
    }

    /// `MM/DD HH:MM` or `MM/DD - MM/DD HH:MM`.
    pub fn date_range(&self) -> String {
        format_date_range(self.created, self.modified)
    }

    /// `Oct 26 → today 09:58` (created → modified, as in the search
    /// preview header), or empty.
    pub fn span_label(&self) -> String {
        format_span(self.created, self.modified, Local::now().date_naive())
    }
}

fn unix(t: SystemTime) -> Option<i64> {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
}

fn local(ts: Option<i64>) -> Option<DateTime<Local>> {
    ts.filter(|&t| t >= MIN_VALID_TS)
        .and_then(|t| DateTime::from_timestamp(t, 0))
        .map(|d| d.with_timezone(&Local))
}

/// See [`SessionView::date_range`].
pub fn format_date_range(start: Option<i64>, end: Option<i64>) -> String {
    let day = |d: &DateTime<Local>| d.format("%m/%d").to_string();
    let time = |d: &DateTime<Local>| d.format("%H:%M").to_string();
    match (local(start), local(end)) {
        (Some(s), Some(e)) if day(&s) == day(&e) => format!("{} {}", day(&e), time(&e)),
        (Some(s), Some(e)) => format!("{} - {} {}", day(&s), day(&e), time(&e)),
        (_, Some(d)) | (Some(d), None) => format!("{} {}", day(&d), time(&d)),
        (None, None) => String::new(),
    }
}

/// See [`SessionView::span_label`]; `today` decides `today HH:MM`.
pub fn format_span(start: Option<i64>, end: Option<i64>, today: NaiveDate) -> String {
    match (local(start), local(end)) {
        (Some(s), Some(e)) => format!("{} → {}", when(s, today), when(e, today)),
        (_, Some(d)) | (Some(d), None) => when(d, today),
        (None, None) => String::new(),
    }
}

/// `today HH:MM`, `Oct 24` (this year) or `Oct 24 2024` — the search
/// preview's date format.
pub fn when(d: DateTime<Local>, today: NaiveDate) -> String {
    if d.date_naive() == today {
        d.format("today %H:%M").to_string()
    } else if d.year() == today.year() {
        d.format("%b %d").to_string()
    } else {
        d.format("%b %d %Y").to_string()
    }
}

/// Read the expensive fields of `view` (whole-file scans).
pub fn load_details(view: &SessionView, homes: &Homes) -> Details {
    let messages = Transcript::load(&view.path, view.agent)
        .map(|t| t.display_messages())
        .unwrap_or_default();
    let real_text = |m: &&crate::session::Message| {
        m.kind == MessageKind::Text && !m.is_meta && !m.text.trim().is_empty()
    };
    let preview = match view.preview_kind {
        PreviewKind::LastUser => messages
            .iter()
            .rev()
            .filter(real_text)
            .find(|m| m.role == Role::User)
            .map(|m| preview_text(&m.text))
            .unwrap_or_default(),
        PreviewKind::LastMessage => messages
            .iter()
            .rev()
            .filter(real_text)
            .find(|m| matches!(m.role, Role::User | Role::Assistant))
            .map(|m| {
                let text: String = m.text.chars().take(200).collect();
                format!("[{}] {}", m.role.as_str(), text.replace('\n', " "))
            })
            .unwrap_or_default(),
    };
    Details {
        user_messages: count_user_messages(&view.path, view.agent),
        preview,
        title: title(view, homes),
        lineage: lineage(&view.path),
    }
}

fn title(view: &SessionView, homes: &Homes) -> Option<String> {
    let sf = SessionFile::read(&view.path, view.agent, Depth::Full).ok()?;
    let sf = match view.agent {
        Agent::Codex => sf.with_codex_name(&codex_thread_names(&homes.codex)),
        Agent::Claude | Agent::Pi | Agent::OpenCode => sf,
    };
    sf.name.filter(|n| !n.is_empty())
}

/// Lineage for the menu: the full chain newest first, or empty when the
/// session is an original.
pub fn lineage(path: &Path) -> Vec<LineageNode> {
    let chain = full_chain(path);
    if chain.len() == 1 && chain[0].1 == Derivation::Original {
        return Vec::new();
    }
    chain
        .into_iter()
        .map(|(p, d)| LineageNode {
            file: crate::commands::file_name(&p),
            derivation: d,
        })
        .collect()
}

#[cfg(test)]
pub mod fixture {
    use super::*;

    /// A minimal view for state/render tests.
    pub fn view(agent: Agent, id: &str) -> SessionView {
        SessionView {
            agent,
            session_id: id.to_string(),
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            cwd: Some("/work/proj".into()),
            project: "proj".into(),
            branch: Some("main".into()),
            is_sidechain: false,
            derivation: Derivation::Original,
            created: Some(1_761_500_000),
            modified: Some(1_761_503_600),
            first_message: Some("set up the spec harness".into()),
            first_title: Some("set up the spec harness".into()),
            default_export_path: PathBuf::from("/work/proj/exported-sessions/claude/x.txt"),
            preview_kind: PreviewKind::LastUser,
            details: Some(Details {
                user_messages: 3,
                preview: "fix the failing spec".into(),
                title: None,
                lineage: Vec::new(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    #[test]
    fn date_range_formats() {
        assert_eq!(format_date_range(None, None), "");
        assert_eq!(format_date_range(Some(0), Some(5)), "");
        let a = 1_761_500_000;
        let same = format_date_range(Some(a), Some(a + 60));
        assert_eq!(same.len(), "MM/DD HH:MM".len());
        let span = format_date_range(Some(a), Some(a + 3 * 86_400));
        assert_eq!(span.len(), "MM/DD - MM/DD HH:MM".len());
        assert_eq!(format_date_range(Some(a), None).len(), 11);
    }

    #[test]
    fn span_formats() {
        let a = 1_761_500_000;
        let day = |t: i64| local(Some(t)).unwrap().date_naive();
        assert_eq!(format_span(None, None, day(a)), "");
        let other = format_span(Some(a), Some(a + 3 * 86_400), day(a + 30 * 86_400));
        assert!(other.contains(" → ") && !other.contains("today"), "{other}");
        assert_eq!(other.len(), "Oct 26 → Oct 29".len());
        let today = format_span(Some(a - 86_400), Some(a), day(a));
        assert!(today.contains(" → today "), "{today}");
        assert!(format_span(None, Some(a), day(a)).starts_with("today "));
        let next_year = day(a + 400 * 86_400);
        assert_eq!(
            format_span(Some(a), None, next_year).len(),
            "Oct 26 2025".len()
        );
    }

    #[test]
    fn first_message_tags_stripped() {
        let d = tempfile::tempdir().unwrap();
        let homes = Homes::new(d.path().join("claude"), d.path().join("codex"));
        let raw =
            r#"[Image #1] <pasted_content id=\"0291\">\nsee https://x.dev/docs\n</pasted_content>"#;
        let lines: Vec<String> = claude_lines(CLAUDE_ID, "/w/proj")
            .into_iter()
            .map(|l| l.replace("fix the failing spec in user_spec.rb", raw))
            .collect();
        let p = write_claude(&homes.claude, CLAUDE_ID, "/w/proj", &lines);
        let sf = SessionFile::read(&p, Agent::Claude, Depth::Header).unwrap();
        let v = SessionView::from_file(&sf, PreviewKind::LastUser);
        assert_eq!(v.first_title.as_deref(), Some("see https://x.dev/docs"));
        assert_eq!(
            v.first_message.as_deref(),
            Some("[Image #1] see https://x.dev/docs")
        );
    }

    #[test]
    fn lines_label_pluralises() {
        let mut v = fixture::view(Agent::Claude, CLAUDE_ID);
        assert_eq!(v.lines_label(), "3 user msgs");
        v.details.as_mut().unwrap().user_messages = 1;
        assert_eq!(v.lines_label(), "1 user msg");
        v.details = None;
        assert_eq!(v.lines_label(), "");
    }

    #[test]
    fn annotation() {
        let mut v = fixture::view(Agent::Claude, CLAUDE_ID);
        assert_eq!(v.annotation(), "");
        v.derivation = Derivation::Trimmed;
        v.is_sidechain = true;
        assert_eq!(v.annotation(), "(t,sub)");
        v.derivation = Derivation::Continued;
        v.is_sidechain = false;
        assert_eq!(v.annotation(), "(r)");
    }

    #[test]
    fn details_from_fixtures() {
        let d = tempfile::tempdir().unwrap();
        let homes = Homes::new(d.path().join("claude"), d.path().join("codex"));
        let p = write_claude(
            &homes.claude,
            CLAUDE_ID,
            "/w/proj",
            &claude_lines(CLAUDE_ID, "/w/proj"),
        );
        let sf = SessionFile::read(&p, Agent::Claude, Depth::Header).unwrap();
        let v = SessionView::from_file(&sf, PreviewKind::LastUser);
        assert!(v.first_message.is_some());
        assert!(v.first_title.is_some());
        assert_eq!(
            (v.project.as_str(), v.branch.as_deref()),
            ("proj", Some("main"))
        );
        let det = load_details(&v, &homes);
        assert_eq!(det.preview, "fix the failing spec in user_spec.rb");
        assert_eq!(det.title.as_deref(), Some("renamed-later"));
        assert!(det.lineage.is_empty());
        assert_eq!(det.user_messages, 2);

        let v = SessionView::from_file(&sf, PreviewKind::LastMessage);
        assert_eq!(load_details(&v, &homes).preview, "[assistant] Fixed.");

        let p = write_codex(
            &homes.codex,
            CODEX_ID,
            "2026-01-02T16-57-30",
            &codex_lines(CODEX_ID, "/w/proj", "cli"),
        );
        let sf = SessionFile::read(&p, Agent::Codex, Depth::Header).unwrap();
        let v = SessionView::from_file(&sf, PreviewKind::LastUser);
        assert_eq!(load_details(&v, &homes).preview, "hello codex");
    }
}
