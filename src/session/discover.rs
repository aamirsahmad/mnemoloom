//! Enumerate sessions under the agent homes (parallel scan): Claude / Codex /
//! pi transcripts, plus opencode DB rows (virtual paths).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use rayon::prelude::*;
use serde_json::Value;

use super::encode::project_dir_candidates;
use super::meta::{Depth, SessionFile};
use super::{Agent, Homes, opencode, pi};

/// Timeout for `git rev-parse` (a slow fsmonitor or network mount).
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// What [`discover`] enumerates.
#[derive(Debug, Clone, Copy)]
pub struct DiscoverOptions {
    /// Restrict to one agent (`None` = all).
    pub agent: Option<Agent>,
    /// Also include Claude sub-agent transcripts nested in session sidecar
    /// dirs (`projects/<enc>/<uuid>/subagents/agent-*.jsonl`); the search
    /// index wants them, resolution does not.
    pub include_subagents: bool,
    pub depth: Depth,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        DiscoverOptions {
            agent: None,
            include_subagents: false,
            depth: Depth::Full,
        }
    }
}

/// Kind of a directory entry, following symlinks (Python `glob` follows
/// them). Broken links are `None`.
enum Kind {
    Dir,
    File,
}

fn kind(e: &fs::DirEntry) -> Option<Kind> {
    let t = e.file_type().ok()?;
    let t = if t.is_symlink() {
        fs::metadata(e.path()).ok()?.file_type()
    } else {
        t
    };
    if t.is_dir() {
        Some(Kind::Dir)
    } else if t.is_file() {
        Some(Kind::File)
    } else {
        None
    }
}

/// Claude transcripts: `projects/*/*.jsonl`, plus everything nested
/// (`projects/**/*.jsonl`) when `include_subagents`. Symlinked project dirs
/// and files are followed (loops guarded).
pub fn claude_session_paths(home: &Path, include_subagents: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(projects) = fs::read_dir(home.join("projects")) else {
        return out;
    };
    let mut visited = std::collections::HashSet::new();
    for project in projects.flatten() {
        if !matches!(kind(&project), Some(Kind::Dir)) {
            continue;
        }
        let Ok(entries) = fs::read_dir(project.path()) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            match kind(&e) {
                Some(Kind::File) if is_jsonl(&p) => out.push(p),
                Some(Kind::Dir) if include_subagents => {
                    walk_jsonl(&p, &mut out, &mut visited, &|p| is_jsonl(p))
                }
                _ => {}
            }
        }
    }
    out
}

/// Codex rollouts: `sessions/**/rollout-*.jsonl` (any depth, symlinks
/// followed).
pub fn codex_session_paths(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut visited = std::collections::HashSet::new();
    walk_jsonl(&home.join("sessions"), &mut out, &mut visited, &|p| {
        is_jsonl(p)
            && p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("rollout-"))
    });
    out
}

fn is_jsonl(p: &Path) -> bool {
    p.extension().is_some_and(|x| x == "jsonl")
}

/// Recursive walk following symlinks; each canonical dir is visited once
/// (cycle guard). Files are kept when `keep` says so.
pub fn walk_jsonl(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    visited: &mut std::collections::HashSet<PathBuf>,
    keep: &dyn Fn(&Path) -> bool,
) {
    let Ok(canon) = fs::canonicalize(dir) else {
        return;
    };
    if !visited.insert(canon) {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match kind(&e) {
            Some(Kind::Dir) => walk_jsonl(&p, out, visited, keep),
            Some(Kind::File) if keep(&p) => out.push(p),
            _ => {}
        }
    }
}

/// Explicit Codex thread names from `<codex_home>/session_index.jsonl`
/// (lowercased id -> name; later entries win; blank skipped).
pub fn codex_thread_names(home: &Path) -> HashMap<String, String> {
    let Ok(bytes) = fs::read(home.join("session_index.jsonl")) else {
        return HashMap::new();
    };
    let mut names = HashMap::new();
    for line in bytes.split(|&b| b == b'\n') {
        let Ok(v) = serde_json::from_slice::<Value>(line.trim_ascii()) else {
            continue;
        };
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let name = v
            .get("thread_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let (Some(id), Some(name)) = (id, name) {
            names.insert(id.to_lowercase(), name.to_string());
        }
    }
    names
}

/// Scan `paths` in parallel; unreadable files are skipped.
pub fn scan_paths(paths: Vec<(Agent, PathBuf)>, depth: Depth) -> Vec<SessionFile> {
    paths
        .into_par_iter()
        .filter_map(|(agent, p)| SessionFile::read(&p, agent, depth).ok())
        .collect()
}

/// opencode sessions in `home` as [`SessionFile`]s (virtual paths), newest
/// first. Unreadable DBs are skipped silently.
pub fn opencode_sessions(home: &Path, depth: Depth) -> Vec<SessionFile> {
    opencode::all_sessions(home)
        .0
        .par_iter()
        .map(|r| opencode::session_file(r, depth))
        .collect()
}

/// Enumerate and scan sessions in every configured home (or one agent's),
/// newest mtime first. Codex names come from `session_index.jsonl`.
/// opencode child sessions count as sub-agents (`include_subagents`).
pub fn discover(homes: &Homes, opts: DiscoverOptions) -> Vec<SessionFile> {
    let wants = |a: Agent| opts.agent.is_none_or(|x| x == a) && homes.has(a);
    let mut paths = Vec::new();
    if wants(Agent::Claude) {
        paths.extend(
            claude_session_paths(&homes.claude, opts.include_subagents)
                .into_iter()
                .map(|p| (Agent::Claude, p)),
        );
    }
    let names = if wants(Agent::Codex) {
        paths.extend(
            codex_session_paths(&homes.codex)
                .into_iter()
                .map(|p| (Agent::Codex, p)),
        );
        codex_thread_names(&homes.codex)
    } else {
        HashMap::new()
    };
    if wants(Agent::Pi) {
        paths.extend(
            pi::session_paths(&homes.pi)
                .into_iter()
                .map(|p| (Agent::Pi, p)),
        );
    }
    let mut out: Vec<SessionFile> = scan_paths(paths, opts.depth)
        .into_iter()
        .map(|sf| sf.with_codex_name(&names))
        .collect();
    if wants(Agent::OpenCode) {
        out.extend(
            opencode_sessions(&homes.opencode, opts.depth)
                .into_iter()
                .filter(|s| opts.include_subagents || !s.is_sidechain),
        );
    }
    out.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// `git rev-parse --abbrev-ref HEAD` in `dir` (None outside a repo or on a
/// timeout).
pub fn current_git_branch(dir: &Path) -> Option<String> {
    let mut git = Command::new("git");
    git.args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(dir);
    let (status, out) = crate::process::run(&mut git, GIT_TIMEOUT).ok()?;
    let b = out.trim().to_string();
    (status.success() && !b.is_empty()).then_some(b)
}

/// One "latest session for this project" candidate.
#[derive(Debug, Clone)]
pub struct Latest {
    pub session: SessionFile,
    /// False when found only via the any-branch fallback.
    pub branch_matched: bool,
}

/// Latest resumable session per agent whose cwd equals `cwd`, preferring
/// `branch` and falling back to any branch. Excludes
/// sub-agents, helpers and `codex exec` runs. Order: Claude, Codex, pi,
/// opencode (unconfigured homes skipped).
///
/// Claude is scanned only in `cwd`'s project dirs (both encodings); Codex
/// and pi scan every header; opencode reads its DB rows.
pub fn latest_for_cwd(
    homes: &Homes,
    agent: Option<Agent>,
    cwd: &Path,
    branch: Option<&str>,
) -> Vec<Latest> {
    let cwd_s = cwd.to_string_lossy();
    let mut out = Vec::new();
    for a in Agent::ALL {
        if agent.is_some_and(|x| x != a) || !homes.has(a) {
            continue;
        }
        let tagged = |paths: Vec<PathBuf>| paths.into_iter().map(|p| (a, p)).collect();
        let scanned = match a {
            Agent::Claude => scan_paths(
                project_dir_candidates(&homes.claude, &cwd_s)
                    .iter()
                    .filter_map(|d| fs::read_dir(d).ok())
                    .flat_map(|rd| rd.flatten().map(|e| e.path()))
                    .filter(|p| is_jsonl(p) && p.is_file())
                    .map(|p| (a, p))
                    .collect(),
                Depth::Header,
            ),
            Agent::Codex => scan_paths(tagged(codex_session_paths(&homes.codex)), Depth::Header),
            Agent::Pi => scan_paths(tagged(pi::session_paths(&homes.pi)), Depth::Header),
            Agent::OpenCode => opencode_sessions(&homes.opencode, Depth::Header),
        };
        let mut found: Vec<SessionFile> = scanned
            .into_iter()
            .filter(|s| s.eligible() && !s.is_sidechain && !s.is_helper && !s.is_exec_run)
            .filter(|s| s.cwd.as_deref() == Some(&*cwd_s))
            .collect();
        found.sort_by_key(|s| std::cmp::Reverse(s.modified));
        let on_branch = branch.and_then(|b| {
            found
                .iter()
                .position(|s| s.git_branch.as_deref() == Some(b))
        });
        let pick = match (on_branch, branch) {
            (Some(i), _) => Some((i, true)),
            (None, None) if !found.is_empty() => Some((0, true)),
            (None, _) if !found.is_empty() => Some((0, false)),
            _ => None,
        };
        if let Some((i, branch_matched)) = pick {
            let mut session = found.swap_remove(i);
            if a == Agent::Codex {
                session = session.with_codex_name(&codex_thread_names(&homes.codex));
            }
            out.push(Latest {
                session,
                branch_matched,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    #[test]
    fn discovers_both_agents_sorted() {
        let d = tempfile::tempdir().unwrap();
        let (ch, xh) = (d.path().join("claude"), d.path().join("codex"));
        let c = write_claude(&ch, CLAUDE_ID, "/w/p", &claude_lines(CLAUDE_ID, "/w/p"));
        let sub = write_raw(
            &c.with_extension("")
                .join("subagents")
                .join("agent-a1.jsonl"),
            &claude_lines(CLAUDE_ID, "/w/p"),
        );
        let x = write_codex(
            &xh,
            CODEX_ID,
            "2026-05-07T12-57-30",
            &codex_lines(CODEX_ID, "/w/p", "cli"),
        );
        fs::write(xh.join("session_index.jsonl"), format!("{{\"id\":\"{CODEX_ID}\",\"thread_name\":\"old\"}}\nbad\n{{\"id\":\"{}\",\"thread_name\":\"new name\"}}\n", CODEX_ID.to_uppercase())).unwrap();
        set_mtime(&c, 1_000);
        set_mtime(&x, 2_000);
        let homes = Homes::new(&ch, &xh);

        let all = discover(&homes, DiscoverOptions::default());
        assert_eq!(
            all.iter().map(|s| s.path.clone()).collect::<Vec<_>>(),
            vec![x.clone(), c.clone()]
        );
        assert_eq!(all[0].name.as_deref(), Some("new name"));

        let with_sub = discover(
            &homes,
            DiscoverOptions {
                include_subagents: true,
                ..Default::default()
            },
        );
        assert!(with_sub.iter().any(|s| s.path == sub && s.is_sidechain));

        let only = discover(
            &homes,
            DiscoverOptions {
                agent: Some(Agent::Claude),
                ..Default::default()
            },
        );
        assert_eq!(only.len(), 1);
    }

    #[test]
    fn follows_symlinked_dirs_without_looping() {
        // Regression: symlinked project dirs are followed, but cycles are not.
        let d = tempfile::tempdir().unwrap();
        let elsewhere = d.path().join("elsewhere");
        let real = write_claude(
            &elsewhere,
            CLAUDE_ID,
            "/w/q",
            &claude_lines(CLAUDE_ID, "/w/q"),
        );
        let home = d.path().join("claude");
        fs::create_dir_all(home.join("projects")).unwrap();
        let proj = real.parent().unwrap();
        std::os::unix::fs::symlink(proj, home.join("projects").join(proj.file_name().unwrap()))
            .unwrap();
        // Loop: a sidecar dir linking back to its parent.
        let side = proj.join(CLAUDE_ID).join("subagents");
        write_raw(&side.join("agent-1.jsonl"), &["{}".into()]);
        std::os::unix::fs::symlink(proj, side.join("loop")).unwrap();
        let found = claude_session_paths(&home, true);
        assert!(
            found
                .iter()
                .any(|p| p.ends_with(format!("{CLAUDE_ID}.jsonl")))
        );
        assert!(found.iter().any(|p| p.ends_with("agent-1.jsonl")));
        assert!(found.len() < 10, "loop guarded: {found:?}");

        let xh = d.path().join("codex");
        let x = write_codex(
            &elsewhere,
            CODEX_ID,
            "2026-05-07T12-57-30",
            &codex_lines(CODEX_ID, "/w", "cli"),
        );
        fs::create_dir_all(&xh).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("sessions"), xh.join("sessions")).unwrap();
        assert_eq!(codex_session_paths(&xh).len(), 1);
        let _ = x;
    }

    #[test]
    fn discovers_pi_and_opencode() {
        let d = tempfile::tempdir().unwrap();
        let (pi, oc) = (d.path().join("pi"), d.path().join("opencode"));
        let p = write_pi(
            &pi,
            PI_ID,
            "/home/alice/src/api",
            &pi_lines(PI_ID, "/home/alice/src/api"),
        );
        let db = write_opencode_db(&oc.join("opencode.db"), "/home/alice/src/api", 2_000);
        set_mtime(&p, 1);
        let homes = Homes::new(d.path().join("c"), d.path().join("x"))
            .with_pi(&pi)
            .with_opencode(&oc);
        let all = discover(&homes, DiscoverOptions::default());
        let ids: Vec<(Agent, &str)> = all
            .iter()
            .map(|s| (s.agent, s.session_id.as_str()))
            .collect();
        assert_eq!(ids, [(Agent::OpenCode, OPENCODE_ID), (Agent::Pi, PI_ID)]);
        assert_eq!(all[0].path, opencode::virtual_path(&db, OPENCODE_ID));
        let with_sub = discover(
            &homes,
            DiscoverOptions {
                include_subagents: true,
                agent: Some(Agent::OpenCode),
                ..Default::default()
            },
        );
        assert_eq!(with_sub.len(), 2);
        // Unconfigured homes are skipped, not read from the defaults.
        assert!(
            discover(
                &Homes::new(d.path().join("c"), d.path().join("x")),
                DiscoverOptions::default()
            )
            .is_empty()
        );

        let l = latest_for_cwd(&homes, None, Path::new("/home/alice/src/api"), None);
        let agents: Vec<Agent> = l.iter().map(|l| l.session.agent).collect();
        assert_eq!(agents, [Agent::Pi, Agent::OpenCode]);
        assert_eq!(
            l[1].session.session_id, OPENCODE_ID,
            "child session skipped"
        );
    }

    #[test]
    fn latest_prefers_branch() {
        let d = tempfile::tempdir().unwrap();
        let (ch, xh) = (d.path().join("claude"), d.path().join("codex"));
        let a = write_claude(&ch, CLAUDE_ID, "/w/p", &claude_lines(CLAUDE_ID, "/w/p"));
        let other: Vec<String> = claude_lines(CLAUDE_ID2, "/w/p")
            .into_iter()
            .map(|l| l.replace("\"main\"", "\"dev\""))
            .collect();
        let b = write_claude(&ch, CLAUDE_ID2, "/w/p", &other);
        set_mtime(&a, 1_000);
        set_mtime(&b, 2_000);
        write_codex(
            &xh,
            CODEX_ID,
            "2026-05-07T12-57-30",
            &codex_lines(CODEX_ID, "/w/p", "exec"),
        );
        let x = write_codex(
            &xh,
            CODEX_ID2,
            "2026-05-07T12-57-31",
            &codex_lines(CODEX_ID2, "/w/p", "cli"),
        );
        let homes = Homes::new(&ch, &xh);

        let l = latest_for_cwd(&homes, None, Path::new("/w/p"), Some("main"));
        assert_eq!(l.len(), 2);
        assert_eq!(
            (l[0].session.path.clone(), l[0].branch_matched),
            (a.clone(), true)
        );
        assert_eq!(l[1].session.path, x, "exec run skipped");
        assert!(!l[1].branch_matched);

        let l = latest_for_cwd(&homes, Some(Agent::Claude), Path::new("/w/p"), Some("zzz"));
        assert_eq!((l[0].session.path.clone(), l[0].branch_matched), (b, false));
        assert!(latest_for_cwd(&homes, None, Path::new("/elsewhere"), None).is_empty());
    }
}
