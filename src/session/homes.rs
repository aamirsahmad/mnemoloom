//! Agent home resolution: flag > env var > default.

use std::path::{Path, PathBuf};

use anyhow::bail;

use super::{Agent, absolute};
use crate::exit::Exit;

/// Resolved agent homes for one invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Homes {
    /// Claude config dir (`--claude-home` > `$CLAUDE_CONFIG_DIR` > `~/.claude`).
    pub claude: PathBuf,
    /// Codex home (`--codex-home` > `$CODEX_HOME` > `~/.codex`).
    pub codex: PathBuf,
    /// True when the Claude home came from a flag or `CLAUDE_CONFIG_DIR`
    /// (resume then exports `CLAUDE_CONFIG_DIR`).
    pub claude_overridden: bool,
    /// True when the Codex home came from a flag or `CODEX_HOME`.
    pub codex_overridden: bool,
    /// pi agent dir (`--pi-home` > `$PI_CODING_AGENT_DIR` > `~/.pi/agent`).
    /// Empty = not configured (skipped by discovery / resolution).
    pub pi: PathBuf,
    /// opencode data dir (`--opencode-home` > `$XDG_DATA_HOME/opencode` >
    /// `~/.local/share/opencode`). Empty = not configured.
    pub opencode: PathBuf,
    /// True when the pi home came from a flag or `PI_CODING_AGENT_DIR`.
    pub pi_overridden: bool,
    /// True when the opencode home came from `--opencode-home`.
    pub opencode_overridden: bool,
}

impl Homes {
    /// Resolve both homes to absolute paths (`~` expanded, relative paths
    /// joined to the cwd now, so a later chdir can't change them).
    /// A blank flag is an error (`"Claude home must not be empty."`).
    /// Existence is checked separately ([`validate_home_flags`], run by `main`).
    pub fn resolve(claude_arg: Option<&Path>, codex_arg: Option<&Path>) -> anyhow::Result<Homes> {
        Homes::resolve_all(claude_arg, codex_arg, None, None)
    }

    /// [`Homes::resolve`] for all four agents (pi / opencode flags too).
    pub fn resolve_all(
        claude_arg: Option<&Path>,
        codex_arg: Option<&Path>,
        pi_arg: Option<&Path>,
        opencode_arg: Option<&Path>,
    ) -> anyhow::Result<Homes> {
        for (agent, arg) in [
            (Agent::Claude, claude_arg),
            (Agent::Codex, codex_arg),
            (Agent::Pi, pi_arg),
            (Agent::OpenCode, opencode_arg),
        ] {
            if arg.is_some_and(|a| a.as_os_str().to_string_lossy().trim().is_empty()) {
                bail!("{} home must not be empty.", agent.label());
            }
        }
        Ok(Homes {
            claude: claude_home(claude_arg),
            codex: codex_home(codex_arg),
            claude_overridden: claude_arg.is_some() || env_nonempty("CLAUDE_CONFIG_DIR").is_some(),
            codex_overridden: codex_arg.is_some() || env_nonempty("CODEX_HOME").is_some(),
            pi: pi_home(pi_arg),
            opencode: opencode_home(opencode_arg),
            pi_overridden: pi_arg.is_some() || env_nonempty("PI_CODING_AGENT_DIR").is_some(),
            opencode_overridden: opencode_arg.is_some(),
        })
    }

    /// Explicit Claude / Codex homes (tests, move-account); pi and opencode
    /// are left unconfigured (see [`Homes::with_pi`] / [`Homes::with_opencode`]).
    pub fn new(claude: impl Into<PathBuf>, codex: impl Into<PathBuf>) -> Homes {
        Homes {
            claude: claude.into(),
            codex: codex.into(),
            claude_overridden: true,
            codex_overridden: true,
            pi: PathBuf::new(),
            opencode: PathBuf::new(),
            pi_overridden: false,
            opencode_overridden: false,
        }
    }

    /// Set an explicit pi home.
    pub fn with_pi(mut self, pi: impl Into<PathBuf>) -> Homes {
        self.pi = pi.into();
        self.pi_overridden = true;
        self
    }

    /// Set an explicit opencode home.
    pub fn with_opencode(mut self, opencode: impl Into<PathBuf>) -> Homes {
        self.opencode = opencode.into();
        self.opencode_overridden = true;
        self
    }

    /// Home directory for `agent` (empty when not configured).
    pub fn home(&self, agent: Agent) -> &Path {
        match agent {
            Agent::Claude => &self.claude,
            Agent::Codex => &self.codex,
            Agent::Pi => &self.pi,
            Agent::OpenCode => &self.opencode,
        }
    }

    /// Whether `agent` has a home to look in (pi / opencode may be unset).
    pub fn has(&self, agent: Agent) -> bool {
        !self.home(agent).as_os_str().is_empty()
    }

    /// `<claude>/projects`.
    pub fn claude_projects(&self) -> PathBuf {
        self.claude.join("projects")
    }

    /// `<codex>/sessions`.
    pub fn codex_sessions(&self) -> PathBuf {
        self.codex.join("sessions")
    }

    /// `<pi>/sessions`.
    pub fn pi_sessions(&self) -> PathBuf {
        self.pi.join("sessions")
    }

    /// All homes in canonical form ([`normalize_path`]): one spelling per
    /// directory, for use as persistent keys (search index). Unset homes
    /// stay empty.
    pub fn canonical(&self) -> Homes {
        let norm = |p: &Path| {
            if p.as_os_str().is_empty() {
                PathBuf::new()
            } else {
                normalize_path(p)
            }
        };
        Homes {
            claude: norm(&self.claude),
            codex: norm(&self.codex),
            pi: norm(&self.pi),
            opencode: norm(&self.opencode),
            ..self.clone()
        }
    }
}

/// Canonical form of a path: `fs::canonicalize` of its deepest existing
/// ancestor (symlinks, `.`/`..`, trailing `/` resolved) with the missing
/// remainder appended after folding `.`/`..` lexically.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut lexical = PathBuf::new();
    for c in absolute(&expand_tilde(path)).components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                lexical.pop();
            }
            other => lexical.push(other),
        }
    }
    let mut base = lexical.as_path();
    let mut rest = Vec::new();
    loop {
        if let Ok(c) = std::fs::canonicalize(base) {
            return rest
                .iter()
                .rev()
                .fold(c, |acc: PathBuf, part| acc.join(part));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                base = parent;
            }
            _ => return lexical,
        }
    }
}

/// A home flag (`--claude-home`, ...; given as `(flag, value)` pairs)
/// naming a missing dir or a file is a usage error (exit 2).
/// Blank values are left to [`Homes::resolve`].
pub fn validate_home_flags(flags: &[(&str, Option<&Path>)]) -> anyhow::Result<()> {
    for &(flag, arg) in flags {
        let Some(arg) = arg.filter(|a| !a.as_os_str().to_string_lossy().trim().is_empty()) else {
            continue;
        };
        let p = expand_tilde(arg);
        let problem = if !p.exists() {
            "does not exist"
        } else if !p.is_dir() {
            "is a file"
        } else {
            continue;
        };
        return Err(Exit {
            code: 2,
            message: Some(format!(
                "Usage: mnemoloom [OPTIONS] [COMMAND]\nTry 'mnemoloom --help' for help.\n\nError: Invalid value for '{flag}': Directory '{}' {problem}.",
                arg.display()
            )),
        }
        .into());
    }
    Ok(())
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Claude home: `arg` > `$CLAUDE_CONFIG_DIR` > `~/.claude`, `~` expanded.
pub fn claude_home(arg: Option<&Path>) -> PathBuf {
    pick_home(arg, "CLAUDE_CONFIG_DIR", ".claude")
}

/// Codex home: `arg` > `$CODEX_HOME` > `~/.codex`, `~` expanded.
pub fn codex_home(arg: Option<&Path>) -> PathBuf {
    pick_home(arg, "CODEX_HOME", ".codex")
}

/// pi home: `arg` > `$PI_CODING_AGENT_DIR` > `~/.pi/agent`, `~` expanded.
pub fn pi_home(arg: Option<&Path>) -> PathBuf {
    pick_home(arg, "PI_CODING_AGENT_DIR", ".pi/agent")
}

/// opencode data dir: `arg` > `$XDG_DATA_HOME/opencode` (absolute only, per
/// the XDG spec) > `~/.local/share/opencode`, `~` expanded.
pub fn opencode_home(arg: Option<&Path>) -> PathBuf {
    if let Some(a) = arg.filter(|a| !a.as_os_str().is_empty()) {
        return absolute(&expand_tilde(a));
    }
    let data = env_nonempty("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| user_home().join(".local/share"));
    data.join("opencode")
}

/// Home for `agent` with the same precedence.
pub fn agent_home(agent: Agent, arg: Option<&Path>) -> PathBuf {
    match agent {
        Agent::Claude => claude_home(arg),
        Agent::Codex => codex_home(arg),
        Agent::Pi => pi_home(arg),
        Agent::OpenCode => opencode_home(arg),
    }
}

/// Default home followed by matching profile siblings in directory order.
/// Keep raw spellings (including symlinks); callers choose ordering,
/// filtering and deduplication, and where to insert an env override.
pub fn profile_homes(home: &Path, agent: Agent) -> Vec<PathBuf> {
    let prefix = format!(".{agent}");
    let mut paths = vec![home.join(&prefix)];
    if let Ok(entries) = std::fs::read_dir(home) {
        paths.extend(
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
                .map(|e| e.path()),
        );
    }
    paths
}

fn pick_home(arg: Option<&Path>, env: &str, default: &str) -> PathBuf {
    if let Some(a) = arg.filter(|a| !a.as_os_str().is_empty()) {
        return absolute(&expand_tilde(a));
    }
    if let Some(v) = env_nonempty(env) {
        return absolute(&expand_tilde(Path::new(&v)));
    }
    user_home().join(default)
}

/// The user's home directory (`$HOME` / platform default).
pub fn user_home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Expand a leading `~`, `~/…`, or `~user/…` (Python `Path.expanduser`).
/// `~user` resolves to a sibling of the current user's home (e.g.
/// `/home/user`) when that dir exists; otherwise the path is unchanged.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    let Some(rest) = s.strip_prefix('~') else {
        return path.to_path_buf();
    };
    let (user, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let base = if user.is_empty() {
        user_home()
    } else {
        match user_home().parent().map(|p| p.join(user)) {
            Some(b) if b.is_dir() => b,
            _ => return path.to_path_buf(),
        }
    };
    if tail.is_empty() {
        base
    } else {
        base.join(tail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_wins_and_tilde_expands() {
        let d = tempfile::tempdir().unwrap();
        let h = Homes::resolve(Some(Path::new("~")), Some(d.path())).unwrap();
        assert_eq!(h.claude, user_home());
        assert_eq!(h.codex, d.path());
        assert!(h.claude_overridden && h.codex_overridden);
        assert_eq!(h.home(Agent::Codex), d.path());
        assert_eq!(expand_tilde(Path::new("~/x")), user_home().join("x"));
        assert_eq!(expand_tilde(Path::new("a/~b")), PathBuf::from("a/~b"));
        assert_eq!(
            expand_tilde(Path::new("~no-such-user-zz/x")),
            PathBuf::from("~no-such-user-zz/x")
        );
        let me = user_home();
        let name = me.file_name().unwrap().to_string_lossy();
        assert_eq!(expand_tilde(Path::new(&format!("~{name}/x"))), me.join("x"));
    }

    #[test]
    fn relative_flag_absolutized_once() {
        // Regression: a relative home must not depend on a later chdir.
        let h = Homes::resolve(Some(Path::new(".")), None).unwrap();
        assert!(h.claude.is_absolute());
        assert_eq!(h.claude, std::env::current_dir().unwrap().join("."));
    }

    #[test]
    fn missing_home_flag_is_usage_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(validate_home_flags(&[("--claude-home", Some(d.path()))]).is_ok());
        let f = d.path().join("f");
        std::fs::write(&f, "").unwrap();
        assert!(validate_home_flags(&[("--pi-home", Some(&f))]).is_err());
        let err = validate_home_flags(&[
            ("--claude-home", None),
            ("--codex-home", Some(Path::new("/definitely/missing"))),
        ])
        .unwrap_err();
        let exit = err.downcast_ref::<Exit>().unwrap();
        assert_eq!(exit.code, 2);
        assert!(exit.message.as_deref().unwrap().ends_with(
            "Error: Invalid value for '--codex-home': Directory '/definitely/missing' does not exist."
        ));
    }

    #[test]
    fn normalized_spellings_agree() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("claude");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let want = normalize_path(&real);
        for p in [
            format!("{}/", real.display()),
            format!("{}/./claude", dir.path().display()),
            format!("{}/claude/../claude", dir.path().display()),
            link.display().to_string(),
        ] {
            assert_eq!(normalize_path(Path::new(&p)), want, "{p}");
        }
        assert_eq!(
            normalize_path(Path::new("/nope/a/../b/")),
            PathBuf::from("/nope/b")
        );
    }

    #[test]
    fn blank_flag_is_error() {
        let err = Homes::resolve(Some(Path::new(" ")), None).unwrap_err();
        assert_eq!(err.to_string(), "Claude home must not be empty.");
        let err = Homes::resolve_all(None, None, None, Some(Path::new(""))).unwrap_err();
        assert_eq!(err.to_string(), "opencode home must not be empty.");
    }

    #[test]
    fn pi_and_opencode_homes() {
        let d = tempfile::tempdir().unwrap();
        let (pi, oc) = (d.path().join("pi"), d.path().join("oc"));
        let h = Homes::resolve_all(None, None, Some(&pi), Some(&oc)).unwrap();
        assert_eq!((h.home(Agent::Pi), h.home(Agent::OpenCode)), (&*pi, &*oc));
        assert!(h.pi_overridden && h.opencode_overridden);
        let n = Homes::new("/c", "/x");
        assert!(!n.has(Agent::Pi) && !n.has(Agent::OpenCode) && n.has(Agent::Claude));
        assert_eq!(n.canonical().pi, PathBuf::new());
        let n = n.with_pi(&pi).with_opencode(&oc);
        assert!(n.has(Agent::Pi) && n.pi_overridden && n.opencode_overridden);
        assert!(opencode_home(None).ends_with("opencode"));
        assert!(pi_home(Some(Path::new("~/p"))).ends_with("p"));
    }
}
