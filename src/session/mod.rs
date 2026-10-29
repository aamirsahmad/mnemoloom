//! Session-file foundation shared by every command.
//!
//! - [`homes`]: Claude/Codex/pi/opencode home resolution.
//! - [`encode`]: cwd <-> Claude project-dir encoding.
//! - [`meta`]: fast per-file metadata ([`SessionFile`]).
//! - [`discover`]: parallel enumeration and "latest session" helpers.
//! - [`transcript`]: full line-level load/rewrite with raw bytes preserved.
//! - [`message`]: common [`Message`] model over both formats.
//! - [`resolve`]: query -> session resolution (tiers, ambiguity, picker).
//! - [`codexdb`]: Codex `state_<N>.sqlite` thread rows.
//! - [`pi`]: pi JSONL sessions (header, entry tree, active branch).
//! - [`opencode`]: opencode SQLite sessions (read-only) and virtual paths.
//! - [`lineage`]: trim/rollover parent chains and injected lineage text.
//! - [`launch`]: resume primitives (`claude -r` / `codex resume`).
//! - [`paths`]: the shared cache root (`$MNEMOLOOM_CACHE_DIR`, XDG, `~/.cache`).
//! - [`pyjson`]: Python `json.dumps`-compatible serialization.
//! - [`timefmt`]: Python-compatible timestamp formatting.

pub mod codexdb;
pub mod discover;
pub mod encode;
pub mod homes;
pub mod launch;
pub mod lineage;
pub mod message;
pub mod meta;
pub mod opencode;
pub mod paths;
pub mod pi;
pub mod pyjson;
pub mod resolve;
pub mod timefmt;
pub mod transcript;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub use discover::{DiscoverOptions, discover, latest_for_cwd};
pub use homes::Homes;
pub use lineage::{Derivation, ParentLink};
pub use message::{Message, MessageKind, Role};
pub use meta::{Depth, SessionFile};
pub use resolve::{Resolution, ResolveOptions, resolve};
pub use transcript::Transcript;

/// Which coding agent produced a session file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Agent {
    Claude,
    Codex,
    /// pi (pi.dev): JSONL entry trees under `<pi home>/sessions`.
    Pi,
    /// opencode: rows in `<opencode home>/opencode*.db` (no per-session file).
    OpenCode,
}

impl Agent {
    /// Every agent, in display / search order.
    pub const ALL: [Agent; 4] = [Agent::Claude, Agent::Codex, Agent::Pi, Agent::OpenCode];

    /// Lowercase wire name: `"claude"` / `"codex"` / `"pi"` / `"opencode"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Pi => "pi",
            Agent::OpenCode => "opencode",
        }
    }

    /// Display label: `"Claude"` / `"Codex"` / `"pi"` / `"opencode"`.
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::Pi => "pi",
            Agent::OpenCode => "opencode",
        }
    }

    /// Three-cell badge: `CLD` / `CDX` / `PI ` / `OPC`.
    pub fn badge(self) -> &'static str {
        match self {
            Agent::Claude => "CLD",
            Agent::Codex => "CDX",
            Agent::Pi => "PI ",
            Agent::OpenCode => "OPC",
        }
    }

    /// The other agent (port / cross-agent rollover target). pi and
    /// opencode sessions can't be ported or rolled over (see
    /// [`Agent::require_rewritable`]); they map to Claude.
    pub fn other(self) -> Agent {
        match self {
            Agent::Claude => Agent::Codex,
            Agent::Codex | Agent::Pi | Agent::OpenCode => Agent::Claude,
        }
    }

    /// Whether mnemoloom can rewrite / copy / move this agent's sessions
    /// (trim, rollover, port, clone, copy, move, delete). Claude and Codex only.
    pub fn is_rewritable(self) -> bool {
        matches!(self, Agent::Claude | Agent::Codex)
    }

    /// Refuse `command` for agents whose sessions can't be rewritten:
    /// `Error: "<command>" is not supported for <agent> sessions yet.` (exit 1).
    pub fn require_rewritable(self, command: &str) -> anyhow::Result<()> {
        if self.is_rewritable() {
            return Ok(());
        }
        Err(crate::exit::Exit::fail(format!(
            "Error: \"{command}\" is not supported for {} sessions yet.",
            self.label()
        ))
        .into())
    }

    /// Parse case-insensitively; `None` for anything else.
    pub fn parse(s: &str) -> Option<Agent> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "pi" => Some(Agent::Pi),
            "opencode" => Some(Agent::OpenCode),
            _ => None,
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

static CODEX_FILENAME_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^rollout-\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-(.+)\.jsonl$").unwrap()
});

/// Session UUID from a file name or stem: strip `.jsonl`; the last 36 chars
/// if the stem is at least that long, else the whole stem. An opencode
/// virtual name (`opencode.db#<id>`) yields the id.
pub fn session_uuid(filename_or_stem: &str) -> String {
    if let Some((_, id)) = filename_or_stem
        .rsplit_once('#')
        .filter(|(_, id)| !id.is_empty())
    {
        return id.to_string();
    }
    let stem = filename_or_stem
        .strip_suffix(".jsonl")
        .unwrap_or(filename_or_stem);
    let n = stem.chars().count();
    if n >= 36 {
        stem.chars().skip(n - 36).collect()
    } else {
        stem.to_string()
    }
}

/// Session UUID of a path (see [`session_uuid`]); the id part of an
/// opencode virtual path (`<db>#<id>`, see [`opencode::split_virtual`]).
pub fn path_session_uuid(path: &Path) -> String {
    if let Some((_, id)) = opencode::split_virtual(path) {
        return id;
    }
    session_uuid(&path.file_name().unwrap_or_default().to_string_lossy())
}

/// File stem of a session path; the session id for an opencode virtual path.
pub fn session_stem(path: &Path) -> String {
    match opencode::split_virtual(path) {
        Some((_, id)) => id,
        None => path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    }
}

/// What the agent's resume flag takes: the transcript path for pi
/// (`pi --session <file>`), the session id otherwise.
pub fn resume_target(agent: Agent, path: &Path) -> String {
    match agent {
        Agent::Pi => path.to_string_lossy().into_owned(),
        _ => path_session_uuid(path),
    }
}

/// Codex rollout id from `rollout-YYYY-MM-DDThh-mm-ss-<id>.jsonl`.
pub fn codex_filename_id(filename: &str) -> Option<String> {
    CODEX_FILENAME_RE
        .captures(filename)
        .map(|c| c[1].to_string())
}

/// `<cwd or process cwd>/exported-sessions/<agent>/<file stem>.txt`.
pub fn default_export_path(session_file: &Path, agent: Agent, cwd: Option<&str>) -> PathBuf {
    let base = cwd
        .filter(|c| !c.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let stem = session_stem(session_file);
    base.join("exported-sessions")
        .join(agent.as_str())
        .join(format!("{stem}.txt"))
}

/// Make a path absolute against the process cwd without touching the
/// filesystem (Python `Path.absolute()`).
pub fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(test)]
pub(crate) mod testutil;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_from_names() {
        let id = "019c0dec-0001-7000-8000-0000000000c5";
        assert_eq!(session_uuid(&format!("{id}.jsonl")), id);
        assert_eq!(
            session_uuid(&format!("rollout-2026-05-07T12-57-30-{id}.jsonl")),
            id
        );
        assert_eq!(session_uuid("short"), "short");
        assert_eq!(
            session_uuid("opencode.db#ses_0a1b2c3d4e5fParentAAAAAAAA"),
            "ses_0a1b2c3d4e5fParentAAAAAAAA"
        );
        assert_eq!(
            codex_filename_id(&format!("rollout-2026-05-07T12-57-30-{id}.jsonl")).as_deref(),
            Some(id)
        );
        assert_eq!(codex_filename_id("rollout-x.jsonl"), None);
    }

    #[test]
    fn agent_parse() {
        assert_eq!(Agent::parse("CODEX"), Some(Agent::Codex));
        assert_eq!(Agent::parse("x"), None);
        assert_eq!(Agent::Claude.other(), Agent::Codex);
        for a in Agent::ALL {
            assert_eq!(Agent::parse(a.as_str()), Some(a));
            assert_eq!(a.badge().chars().count(), 3);
        }
        assert_eq!(Agent::parse("OpenCode"), Some(Agent::OpenCode));
        assert!(Agent::Codex.require_rewritable("trim").is_ok());
        let err = Agent::Pi.require_rewritable("trim").unwrap_err();
        let exit = err.downcast_ref::<crate::exit::Exit>().unwrap();
        assert_eq!(
            exit.message.as_deref(),
            Some("Error: \"trim\" is not supported for pi sessions yet.")
        );
    }

    #[test]
    fn export_path() {
        let p = default_export_path(
            Path::new("/h/projects/x/abc.jsonl"),
            Agent::Claude,
            Some("/w"),
        );
        assert_eq!(p, PathBuf::from("/w/exported-sessions/claude/abc.txt"));
        let v = Path::new("/d/opencode.db#ses_abc");
        assert_eq!(
            default_export_path(v, Agent::OpenCode, Some("/w")),
            PathBuf::from("/w/exported-sessions/opencode/ses_abc.txt")
        );
        assert_eq!(path_session_uuid(v), "ses_abc");
        let pi =
            Path::new("/p/2026-01-01T00-00-00-000Z_019c0dec-0001-7000-8000-0000000000c5.jsonl");
        assert_eq!(resume_target(Agent::Pi, pi), pi.to_string_lossy());
        assert_eq!(
            resume_target(Agent::Codex, pi),
            "019c0dec-0001-7000-8000-0000000000c5"
        );
    }
}
