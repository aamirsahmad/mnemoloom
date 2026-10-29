//! Session resolution: name / full id / id fragment / filename fragment /
//! path -> one session, with six match tiers and ambiguity rules.
//!
//! Layers:
//! - [`resolve_in_home`]: one agent, one home (the `resolve` command's JSON
//!   contract: single / ambiguous / not_found).
//! - [`resolve_query`]: direct paths + both agents, cross-agent tier
//!   precedence, content-id validation.
//! - [`resolve`]: CLI wrapper — numbered picker on a TTY, otherwise
//!   `Error: ...` + exit 1 via [`crate::exit::Exit`].

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::SystemTime;

use serde_json::{Value, json};

use super::codexdb::{ThreadRow, read_threads, state_database};
use super::discover::{claude_session_paths, codex_session_paths, codex_thread_names, scan_paths};
use super::homes::{agent_home, expand_tilde};
use super::meta::{Depth, SessionFile, detect_agent};
use super::timefmt::iso_local_offset;
use super::{Agent, Homes, absolute, codex_filename_id, opencode, path_session_uuid, pi};
use crate::exit::Exit;

/// Ambiguity listings / pickers show at most this many candidates.
pub const MAX_CANDIDATES: usize = 25;
const MAX_NAME_CHARS: usize = 60;

static PARTIAL_ID_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)^[0-9a-f-]+$").unwrap());

/// Which tier matched (`matched_by` in JSON).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Id,
    PartialId,
    IdSubstring,
    Name,
    Filename,
}

impl MatchKind {
    /// `id` / `partial-id` / `id-substring` / `name` / `filename`.
    pub fn as_str(self) -> &'static str {
        match self {
            MatchKind::Id => "id",
            MatchKind::PartialId => "partial-id",
            MatchKind::IdSubstring => "id-substring",
            MatchKind::Name => "name",
            MatchKind::Filename => "filename",
        }
    }
}

/// One resolvable session as seen by the resolver.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub agent: Agent,
    pub session_id: String,
    pub name: Option<String>,
    /// Session cwd.
    pub directory: Option<String>,
    pub home: PathBuf,
    pub session_file: PathBuf,
    pub matched_by: Option<MatchKind>,
    /// Local ISO mtime with offset.
    pub modified: String,
    pub modified_time: SystemTime,
    pub archived: bool,
    /// Eligible for every tier (not just exact id).
    pub eligible: bool,
    pub content_id: Option<String>,
    pub git_branch: Option<String>,
}

impl SessionRecord {
    /// Record for a scanned file (`matched_by` unset).
    pub fn from_session(sf: &SessionFile, home: &Path) -> SessionRecord {
        let eligible = sf.eligible();
        SessionRecord {
            agent: sf.agent,
            session_id: sf.session_id.clone(),
            name: match sf.agent {
                Agent::Claude if eligible => sf.name.clone(),
                Agent::Claude => None,
                Agent::Codex | Agent::Pi | Agent::OpenCode => sf.name.clone(),
            },
            directory: sf.cwd.clone(),
            home: home.to_path_buf(),
            session_file: sf.path.clone(),
            matched_by: None,
            modified: iso_local_offset(sf.modified),
            modified_time: sf.modified,
            archived: false,
            eligible,
            content_id: sf.content_id.clone(),
            git_branch: sf.git_branch.clone(),
        }
    }

    /// Public JSON shape.
    pub fn to_json(&self) -> Value {
        json!({
            "agent": self.agent.as_str(),
            "session_id": self.session_id,
            "name": self.name,
            "directory": self.directory,
            "home": self.home.to_string_lossy(),
            "session_file": self.session_file.to_string_lossy(),
            "matched_by": self.matched_by.map(MatchKind::as_str),
            "modified": self.modified,
            "archived": self.archived,
        })
    }

    /// Picker reason (`exact session ID`, `name/title contains query`, ...).
    pub fn match_reason(&self, query: &str) -> &'static str {
        match self.matched_by {
            Some(MatchKind::Id) => "exact session ID",
            Some(MatchKind::PartialId) => "session ID prefix",
            Some(MatchKind::IdSubstring) => "session ID contains query",
            Some(MatchKind::Filename) => "filename contains query",
            Some(MatchKind::Name)
                if self
                    .name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase() == query.to_lowercase()) =>
            {
                "exact name/title"
            }
            Some(MatchKind::Name) => "name/title contains query",
            None => "resolver match",
        }
    }

    fn tier(&self, query: &str) -> u8 {
        match self.matched_by {
            Some(MatchKind::Id) => 0,
            Some(MatchKind::Name) if self.match_reason(query) == "exact name/title" => 1,
            Some(MatchKind::PartialId) => 2,
            Some(MatchKind::IdSubstring) => 3,
            Some(MatchKind::Filename) => 4,
            _ => 5,
        }
    }
}

/// Expected resolver failure with a stable machine code
/// (`invalid_query`, `invalid_home`, `unreadable_home`, `invalid_agent`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverError {
    pub code: String,
    pub detail: String,
}

impl ResolverError {
    pub fn new(code: &str, detail: impl Into<String>) -> Self {
        ResolverError {
            code: code.to_string(),
            detail: detail.into(),
        }
    }

    /// `{"error": code, "detail": detail}`.
    pub fn to_json(&self) -> Value {
        json!({"error": self.code, "detail": self.detail})
    }
}

impl std::fmt::Display for ResolverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for ResolverError {}

/// Result of resolving in one home.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Single(SessionRecord),
    /// Newest first, capped at [`MAX_CANDIDATES`]; `match_count` is the total.
    Ambiguous {
        records: Vec<SessionRecord>,
        match_count: usize,
    },
    NotFound,
}

/// [`resolve_in_home`] result.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeResolution {
    pub query: String,
    pub agent: Agent,
    pub home: PathBuf,
    pub outcome: Outcome,
}

impl HomeResolution {
    /// `resolve --json` payload.
    pub fn to_json(&self) -> Value {
        match &self.outcome {
            Outcome::Single(r) => r.to_json(),
            Outcome::Ambiguous {
                records,
                match_count,
            } => json!({
                "error": "ambiguous",
                "query": self.query,
                "agent": self.agent.as_str(),
                "match_count": match_count,
                "candidates": records.iter().map(SessionRecord::to_json).collect::<Vec<_>>(),
            }),
            Outcome::NotFound => json!({
                "error": "not_found",
                "query": self.query,
                "agent": self.agent.as_str(),
                "home": self.home.to_string_lossy(),
            }),
        }
    }

    fn records(&self) -> &[SessionRecord] {
        match &self.outcome {
            Outcome::Single(r) => std::slice::from_ref(r),
            Outcome::Ambiguous { records, .. } => records,
            Outcome::NotFound => &[],
        }
    }

    fn match_count(&self) -> usize {
        match &self.outcome {
            Outcome::Single(_) => 1,
            Outcome::Ambiguous { match_count, .. } => *match_count,
            Outcome::NotFound => 0,
        }
    }
}

/// Canonicalize when possible, else make absolute (Python `resolve(strict=False)`).
fn resolve_lenient(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| absolute(p))
}

/// Home for `agent` (`arg` > env > default), canonicalized. A blank `arg`
/// is `invalid_home`.
pub fn resolve_home(agent: Agent, arg: Option<&Path>) -> Result<PathBuf, ResolverError> {
    if arg.is_some_and(|a| a.to_string_lossy().trim().is_empty()) {
        return Err(ResolverError::new(
            "invalid_home",
            "Home must not be empty.",
        ));
    }
    Ok(resolve_lenient(&agent_home(agent, arg)))
}

fn validate_home(home: &Path) -> Result<(), ResolverError> {
    if !home.exists() {
        return Err(ResolverError::new(
            "invalid_home",
            format!("Home does not exist: {}", home.display()),
        ));
    }
    if !home.is_dir() {
        return Err(ResolverError::new(
            "invalid_home",
            format!("Home is not a directory: {}", home.display()),
        ));
    }
    fs::read_dir(home).map(|_| ()).map_err(|e| {
        ResolverError::new(
            "unreadable_home",
            format!("Cannot read home {}: {e}", home.display()),
        )
    })
}

/// Transcript files of a file-based agent (`agent` != opencode).
fn candidate_paths(agent: Agent, home: &Path) -> Result<Vec<PathBuf>, ResolverError> {
    validate_home(home)?;
    let (sub, label) = match agent {
        Agent::Claude => ("projects", "Claude projects path"),
        Agent::Codex => ("sessions", "Codex sessions path"),
        Agent::Pi | Agent::OpenCode => ("sessions", "pi sessions path"),
    };
    let root = home.join(sub);
    if !root.exists() {
        return Ok(Vec::new());
    }
    if !root.is_dir() {
        return Err(ResolverError::new(
            "invalid_home",
            format!("{label} is not a directory: {}", root.display()),
        ));
    }
    Ok(match agent {
        Agent::Claude => claude_session_paths(home, false),
        Agent::Codex => codex_session_paths(home),
        Agent::Pi | Agent::OpenCode => pi::session_paths(home),
    })
}

/// Keep the newest (eligible first) record per id / path.
fn dedup(mut records: Vec<SessionRecord>) -> Vec<SessionRecord> {
    records.sort_by_key(|r| std::cmp::Reverse((r.eligible, r.modified_time)));
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    records.retain(|r| {
        let fresh = !ids.contains(&r.session_id.to_lowercase()) && !paths.contains(&r.session_file);
        if fresh {
            ids.insert(r.session_id.to_lowercase());
            paths.insert(r.session_file.clone());
        }
        fresh
    });
    records
}

/// Claude records (whole-file scan: names need `custom-title` lines).
/// `only_id`: restrict to files whose stem equals this (lowercased) id.
fn claude_records(home: &Path, only_id: Option<&str>) -> Result<Vec<SessionRecord>, ResolverError> {
    let paths = candidate_paths(Agent::Claude, home)?
        .into_iter()
        .filter(|p| only_id.is_none_or(|q| path_session_uuid(p).to_lowercase() == q))
        .map(|p| (Agent::Claude, p))
        .collect();
    let records = scan_paths(paths, Depth::Full)
        .into_iter()
        .filter(|sf| sf.has_conversation)
        .map(|sf| SessionRecord::from_session(&sf, home))
        .collect();
    Ok(dedup(records))
}

/// Codex records: `state_<N>.sqlite` threads (authoritative title / cwd /
/// archived) merged with on-disk rollouts the DB doesn't know, then
/// `session_index.jsonl` names overlaid. `db_fallback`: a broken DB
/// degrades to disk-only instead of failing.
fn codex_records(
    home: &Path,
    only_id: Option<&str>,
    db_fallback: bool,
) -> Result<Vec<SessionRecord>, ResolverError> {
    let disk = candidate_paths(Agent::Codex, home)?;
    let rows = match state_database(home).map(|db| read_threads(&db)) {
        None => Vec::new(),
        Some(Ok(rows)) => rows,
        Some(Err(_)) if db_fallback => Vec::new(),
        Some(Err(e)) => return Err(e),
    };
    let rows: Vec<(ThreadRow, PathBuf)> = rows
        .into_iter()
        .filter(|r| only_id.is_none_or(|q| r.id.to_lowercase() == q))
        .filter_map(|r| {
            let raw = r.rollout_path.as_deref().filter(|p| !p.trim().is_empty())?;
            let p = expand_tilde(Path::new(raw));
            let p = resolve_lenient(&if p.is_absolute() { p } else { home.join(p) });
            let name = p.file_name()?.to_string_lossy().into_owned();
            let ok = p.is_file()
                && codex_filename_id(&name).is_none_or(|f| f.eq_ignore_ascii_case(&r.id));
            ok.then_some((r, p))
        })
        .collect();
    let scanned = |paths: Vec<PathBuf>| -> HashMap<PathBuf, SessionFile> {
        scan_paths(
            paths.into_iter().map(|p| (Agent::Codex, p)).collect(),
            Depth::Header,
        )
        .into_iter()
        .filter(|sf| sf.is_valid)
        .map(|sf| (sf.path.clone(), sf))
        .collect()
    };
    let db_meta = scanned(rows.iter().map(|(_, p)| p.clone()).collect());
    let db_records = dedup(
        rows.iter()
            .filter_map(|(row, p)| {
                let sf = db_meta.get(p)?;
                let mut rec = SessionRecord::from_session(sf, home);
                rec.session_id.clone_from(&row.id);
                rec.name.clone_from(&row.title);
                rec.directory.clone_from(&row.cwd);
                rec.archived = row.archived;
                rec.git_branch = row.git_branch.clone().or(rec.git_branch);
                Some(rec)
            })
            .collect(),
    );
    let known_ids: HashSet<String> = db_records
        .iter()
        .map(|r| r.session_id.to_lowercase())
        .collect();
    let known_paths: HashSet<&PathBuf> = db_records.iter().map(|r| &r.session_file).collect();
    let disk: Vec<PathBuf> = disk
        .into_iter()
        .filter(|p| {
            let Some(id) = codex_filename_id(&p.file_name().unwrap_or_default().to_string_lossy())
            else {
                return false;
            };
            let id = id.to_lowercase();
            !known_ids.contains(&id) && !known_paths.contains(p) && only_id.is_none_or(|q| id == q)
        })
        .collect();
    let disk_records = dedup(
        scanned(disk)
            .values()
            .map(|sf| SessionRecord::from_session(sf, home))
            .collect(),
    );
    let names = codex_thread_names(home);
    Ok(db_records
        .into_iter()
        .chain(disk_records)
        .map(|mut r| {
            if let Some(n) = names.get(&r.session_id.to_lowercase()) {
                r.name = Some(n.clone());
            }
            r
        })
        .collect())
}

/// pi records (whole-file scan: names come from `session_info` entries).
fn pi_records(home: &Path, only_id: Option<&str>) -> Result<Vec<SessionRecord>, ResolverError> {
    let paths = candidate_paths(Agent::Pi, home)?
        .into_iter()
        .filter(|p| only_id.is_none_or(|q| path_session_uuid(p).to_lowercase() == q))
        .map(|p| (Agent::Pi, p))
        .collect();
    let records = scan_paths(paths, Depth::Full)
        .into_iter()
        .filter(|sf| sf.is_valid)
        .map(|sf| SessionRecord::from_session(&sf, home))
        .collect();
    Ok(dedup(records))
}

/// opencode records from every DB in `home` (rows deduplicated by id).
/// `db_fallback`: an unreadable DB is skipped instead of failing.
fn opencode_records(
    home: &Path,
    only_id: Option<&str>,
    db_fallback: bool,
) -> Result<Vec<SessionRecord>, ResolverError> {
    validate_home(home)?;
    let (rows, errors) = opencode::all_sessions(home);
    if let Some((db, e)) = errors.into_iter().next().filter(|_| !db_fallback) {
        return Err(ResolverError::new(
            "unreadable_database",
            format!("Cannot read {}: {e}", db.display()),
        ));
    }
    Ok(rows
        .iter()
        .filter(|r| only_id.is_none_or(|q| r.id.to_lowercase() == q))
        .map(|r| {
            let mut rec =
                SessionRecord::from_session(&opencode::session_file(r, Depth::Header), home);
            rec.archived = r.archived;
            rec
        })
        .collect())
}

fn records(
    agent: Agent,
    home: &Path,
    only_id: Option<&str>,
    db_fallback: bool,
) -> Result<Vec<SessionRecord>, ResolverError> {
    match agent {
        Agent::Claude => claude_records(home, only_id),
        Agent::Codex => codex_records(home, only_id, db_fallback),
        Agent::Pi => pi_records(home, only_id),
        Agent::OpenCode => opencode_records(home, only_id, db_fallback),
    }
}

/// Every session record of `agent` in `home` (resolver view; `home` should
/// already be resolved, see [`resolve_home`]).
pub fn enumerate(agent: Agent, home: &Path) -> Result<Vec<SessionRecord>, ResolverError> {
    records(agent, home, None, false)
}

/// Apply the tiers; returns only the first non-empty tier.
/// 0 exact id (incl. ineligible) · 1 exact name · 2 id prefix · 3 id
/// substring (2–3 need >= 4 chars of `[0-9a-f-]`) · 4 filename substring
/// (>= 4 chars) · 5 name substring. Queries containing `/ \ * ? [ ]`
/// match nothing.
pub fn match_records(
    records: &[SessionRecord],
    query: &str,
) -> (Vec<SessionRecord>, Option<MatchKind>) {
    if query.chars().any(|c| "/\\*?[]".contains(c)) {
        return (Vec::new(), None);
    }
    let q = query.to_lowercase();
    let long = query.chars().count() >= 4;
    let pick = |pred: &dyn Fn(&SessionRecord) -> bool| -> Vec<SessionRecord> {
        records.iter().filter(|r| pred(r)).cloned().collect()
    };
    let name_lc = |r: &SessionRecord| {
        r.name
            .as_deref()
            .filter(|n| !n.is_empty())
            .map(str::to_lowercase)
    };
    type Pred<'a> = Box<dyn Fn(&SessionRecord) -> bool + 'a>;
    let tiers: [(MatchKind, Pred); 6] = [
        (
            MatchKind::Id,
            Box::new(|r| r.session_id.to_lowercase() == q),
        ),
        (
            MatchKind::Name,
            Box::new(|r| r.eligible && name_lc(r).is_some_and(|n| n == q)),
        ),
        (
            MatchKind::PartialId,
            Box::new(|r| {
                long && PARTIAL_ID_RE.is_match(query)
                    && r.eligible
                    && r.session_id.to_lowercase().starts_with(&q)
            }),
        ),
        (
            MatchKind::IdSubstring,
            Box::new(|r| {
                long && PARTIAL_ID_RE.is_match(query)
                    && r.eligible
                    && r.session_id.to_lowercase().contains(&q)
            }),
        ),
        (
            MatchKind::Filename,
            Box::new(|r| {
                long && r.eligible
                    && r.session_file
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().to_lowercase().contains(&q))
            }),
        ),
        (
            MatchKind::Name,
            Box::new(|r| r.eligible && name_lc(r).is_some_and(|n| n.contains(&q))),
        ),
    ];
    for (kind, pred) in &tiers {
        let hits = pick(pred.as_ref());
        if !hits.is_empty() {
            return (hits, Some(*kind));
        }
    }
    (Vec::new(), None)
}

/// Resolve `query` among one agent's sessions in one home (Codex DB
/// errors are reported). Exact ids are
/// answered from file names / DB rows without scanning the whole home.
pub fn resolve_in_home(
    query: &str,
    agent: Agent,
    home_arg: Option<&Path>,
) -> Result<HomeResolution, ResolverError> {
    resolve_in_home_with(query, agent, home_arg, false)
}

fn resolve_in_home_with(
    query: &str,
    agent: Agent,
    home_arg: Option<&Path>,
    db_fallback: bool,
) -> Result<HomeResolution, ResolverError> {
    if query.trim().is_empty() {
        return Err(ResolverError::new(
            "invalid_query",
            "Query must not be empty.",
        ));
    }
    let home = resolve_home(agent, home_arg)?;
    let q = query.to_lowercase();
    let exact = records(agent, &home, Some(&q), db_fallback)?;
    let (mut hits, kind) = if exact.is_empty() {
        match_records(&records(agent, &home, None, db_fallback)?, query)
    } else {
        (exact, Some(MatchKind::Id))
    };
    for h in &mut hits {
        h.matched_by = kind;
    }
    hits.sort_by_key(|r| std::cmp::Reverse(r.modified_time));
    let outcome = match hits.len() {
        0 => Outcome::NotFound,
        1 => Outcome::Single(hits.remove(0)),
        n => {
            hits.truncate(MAX_CANDIDATES);
            Outcome::Ambiguous {
                records: hits,
                match_count: n,
            }
        }
    };
    Ok(HomeResolution {
        query: query.to_string(),
        agent,
        home,
        outcome,
    })
}

/// A uniquely resolved session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub agent: Agent,
    /// Absolute path of the transcript (opencode: the virtual
    /// `<db>#<id>` path, see [`opencode::split_virtual`]).
    pub session_file: PathBuf,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
}

impl Resolution {
    /// Session id from the file name (opencode: from the virtual path).
    pub fn session_id(&self) -> String {
        path_session_uuid(&self.session_file)
    }

    /// What the agent's resume command takes ([`super::resume_target`]).
    pub fn resume_target(&self) -> String {
        super::resume_target(self.agent, &self.session_file)
    }

    /// Read metadata for the resolved file.
    pub fn metadata(&self, depth: Depth) -> io::Result<SessionFile> {
        SessionFile::read(&self.session_file, self.agent, depth)
    }

    fn from_path(agent: Agent, path: PathBuf, cwd: Option<String>) -> Resolution {
        let meta = SessionFile::read(&path, agent, Depth::Header).ok();
        Resolution {
            agent,
            cwd: cwd.or_else(|| meta.as_ref().and_then(|m| m.cwd.clone())),
            git_branch: meta.and_then(|m| m.git_branch),
            session_file: path,
        }
    }

    /// Resolution for a record chosen from an ambiguity.
    pub fn from_record(r: &SessionRecord) -> Resolution {
        Resolution::from_path(r.agent, absolute(&r.session_file), r.directory.clone())
    }
}

/// Failure of [`resolve_query`].
#[derive(Debug, Clone, PartialEq)]
pub enum QueryError {
    /// Several eligible matches (records newest first, capped).
    Ambiguous {
        query: String,
        records: Vec<SessionRecord>,
        match_count: usize,
        message: String,
    },
    /// Not found / undetectable path / agent mismatch / resolver error.
    Failed(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryError::Ambiguous { message, .. } | QueryError::Failed(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for QueryError {}

fn ambiguity_message(query: &str, records: &[SessionRecord], total: usize) -> String {
    let mut lines: Vec<String> = records
        .iter()
        .map(|r| {
            let mut name = r
                .name
                .as_deref()
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if name.chars().count() > MAX_NAME_CHARS {
                name = format!(
                    "{}...",
                    name.chars().take(MAX_NAME_CHARS - 3).collect::<String>()
                );
            }
            let name_part = if name.is_empty() {
                String::new()
            } else {
                format!(" ({name})")
            };
            format!(
                "  [{}] {}{name_part}  modified {}",
                r.agent, r.session_id, r.modified
            )
        })
        .collect();
    if total > records.len() {
        lines.push(format!("  ... and {} more", total - records.len()));
    }
    format!(
        "Ambiguous session '{query}' matches {total} sessions:\n{}\nUse a longer unique id, the exact session name, or the full file path.",
        lines.join("\n")
    )
}

/// Drop records whose content id contradicts their file-name id.
fn validate_content_ids(res: HomeResolution) -> Option<HomeResolution> {
    let before = res.records().len();
    let valid: Vec<SessionRecord> = res
        .records()
        .iter()
        .filter(|r| {
            r.content_id
                .as_deref()
                .is_none_or(|c| c.eq_ignore_ascii_case(&r.session_id))
        })
        .cloned()
        .collect();
    let removed = before - valid.len();
    let remaining = res.match_count() - removed;
    if remaining == 0 {
        return None;
    }
    if valid.is_empty() {
        return Some(res);
    }
    let outcome = if remaining == 1 {
        Outcome::Single(valid.into_iter().next().expect("non-empty"))
    } else {
        Outcome::Ambiguous {
            records: valid,
            match_count: remaining,
        }
    };
    Some(HomeResolution { outcome, ..res })
}

/// Resolve a path, id (fragment), filename fragment or name across the
/// configured homes.
pub fn resolve_query(
    query: &str,
    agent: Option<Agent>,
    homes: &Homes,
) -> Result<Resolution, QueryError> {
    let direct = expand_tilde(Path::new(query));
    if !query.is_empty() && (direct.is_file() || opencode::split_virtual(&direct).is_some()) {
        let detected = detect_agent(&direct, homes).filter(|a| {
            *a == Agent::Claude
                || SessionFile::read(&direct, *a, Depth::Header).is_ok_and(|s| s.is_valid)
        });
        let Some(detected) = detected else {
            return Err(QueryError::Failed(format!(
                "Could not detect agent for session file: {}",
                direct.display()
            )));
        };
        if let Some(a) = agent.filter(|a| *a != detected) {
            return Err(QueryError::Failed(format!(
                "Session file is a {detected} session, but --agent {a} was specified: {}",
                direct.display()
            )));
        }
        return Ok(Resolution::from_path(detected, absolute(&direct), None));
    }

    let mut results = Vec::new();
    let mut errors = Vec::new();
    for a in Agent::ALL {
        if agent.is_some_and(|x| x != a) || !homes.has(a) {
            continue;
        }
        if agent.is_none() && !a.is_rewritable() && !homes.home(a).is_dir() {
            // An absent pi / opencode home is not an error for a search
            // across all agents (Claude / Codex still report it).
            continue;
        }
        match resolve_in_home_with(query, a, Some(homes.home(a)), true) {
            Ok(r) if r.outcome != Outcome::NotFound => results.extend(validate_content_ids(r)),
            Ok(_) => {}
            Err(e) => errors.push(e),
        }
    }
    if let Some(best) = results
        .iter()
        .filter_map(|r| r.records().first().map(|x| x.tier(query)))
        .min()
    {
        results.retain(|r| r.records().first().is_some_and(|x| x.tier(query) == best));
    }
    if let [only] = results.as_slice()
        && let Outcome::Single(rec) = &only.outcome
    {
        return Ok(Resolution {
            agent: rec.agent,
            session_file: rec.session_file.clone(),
            cwd: rec.directory.clone(),
            git_branch: rec.git_branch.clone(),
        });
    }
    if !results.is_empty() {
        let mut records: Vec<SessionRecord> = results
            .iter()
            .flat_map(|r| r.records().iter().cloned())
            .collect();
        records.sort_by_key(|r| std::cmp::Reverse(r.modified_time));
        records.truncate(MAX_CANDIDATES);
        let total = results.iter().map(HomeResolution::match_count).sum();
        let message = ambiguity_message(query, &records, total);
        return Err(QueryError::Ambiguous {
            query: query.to_string(),
            records,
            match_count: total,
            message,
        });
    }
    if let Some(e) = errors.into_iter().next() {
        return Err(QueryError::Failed(e.detail));
    }
    Err(QueryError::Failed(format!("Session not found: {query}")))
}

/// Options for the CLI-level [`resolve`].
#[derive(Debug, Clone)]
pub struct ResolveOptions<'a> {
    pub homes: &'a Homes,
    /// Hard agent constraint (`--agent`).
    pub agent: Option<Agent>,
    /// Offer the numbered picker when stdin is a TTY.
    pub interactive: bool,
    /// Question under the picker (e.g. `Which session do you want to delete?`).
    pub selection_prompt: &'a str,
    /// Also accept a unique exact Claude transcript stem (trim-in-place).
    pub allow_claude_stem: bool,
    /// Destructive commands: an id present in several files is ambiguous
    /// (unless the query is a direct path).
    pub unique_file: bool,
}

impl<'a> ResolveOptions<'a> {
    /// Interactive, both agents, default prompt.
    pub fn new(homes: &'a Homes) -> Self {
        ResolveOptions {
            homes,
            agent: None,
            interactive: true,
            selection_prompt: "Which session do you want to use?",
            allow_claude_stem: false,
            unique_file: false,
        }
    }

    pub fn agent(mut self, agent: Option<Agent>) -> Self {
        self.agent = agent;
        self
    }

    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    pub fn prompt(mut self, prompt: &'a str) -> Self {
        self.selection_prompt = prompt;
        self
    }

    pub fn allow_claude_stem(mut self, allow: bool) -> Self {
        self.allow_claude_stem = allow;
        self
    }

    pub fn unique_file(mut self, unique: bool) -> Self {
        self.unique_file = unique;
        self
    }
}

/// CLI resolution. Ambiguity on a TTY shows
/// the picker; otherwise (and for not-found) returns an [`Exit`] error
/// printing `Error: <message>` with code 1. Cancelling the picker prints
/// `Session selection cancelled.` (exit 1).
pub fn resolve(query: &str, opts: &ResolveOptions) -> anyhow::Result<Resolution> {
    let res = resolve_unchecked(query, opts)?;
    if opts.unique_file && !expand_tilde(Path::new(query)).is_file() {
        return ensure_unique_file(query, res, opts);
    }
    Ok(res)
}

fn resolve_unchecked(query: &str, opts: &ResolveOptions) -> anyhow::Result<Resolution> {
    match resolve_query(query, opts.agent, opts.homes) {
        Ok(r) => Ok(r),
        Err(QueryError::Ambiguous {
            query,
            records,
            match_count,
            message,
        }) => {
            if opts.interactive && io::stdin().is_terminal() {
                match choose_record(&query, &records, match_count, opts.selection_prompt, false)? {
                    Some(i) => Ok(Resolution::from_record(&records[i])),
                    None => Err(Exit::fail("Session selection cancelled.").into()),
                }
            } else {
                Err(Exit::fail(format!("Error: {message}")).into())
            }
        }
        Err(QueryError::Failed(msg)) => {
            if opts.allow_claude_stem
                && opts.agent == Some(Agent::Claude)
                && let Some(p) = exact_claude_filename(query, &opts.homes.claude)
            {
                return Ok(Resolution::from_path(Agent::Claude, p, None));
            }
            Err(Exit::fail(format!("Error: {msg}")).into())
        }
    }
}

/// Unique `projects/*/<stem>.jsonl` for an exact transcript stem.
pub fn exact_claude_filename(stem: &str, claude_home: &Path) -> Option<PathBuf> {
    if stem.is_empty() || Path::new(stem).file_name().is_none_or(|n| n != stem) {
        return None;
    }
    let matches: Vec<PathBuf> = fs::read_dir(claude_home.join("projects"))
        .ok()?
        .flatten()
        .map(|d| d.path().join(format!("{stem}.jsonl")))
        .filter(|p| p.is_file())
        .collect();
    (matches.len() == 1).then(|| absolute(&matches[0]))
}

fn clean_title(t: Option<&str>) -> String {
    let s = t
        .unwrap_or("Untitled session")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if s.chars().count() > 80 {
        format!("{}...", s.chars().take(77).collect::<String>())
    } else {
        s
    }
}

/// Numbered picker on stderr reading stdin. Returns the chosen index, or
/// `None` on `q` / EOF.
pub fn choose_record(
    query: &str,
    records: &[SessionRecord],
    match_count: usize,
    prompt: &str,
    show_home: bool,
) -> io::Result<Option<usize>> {
    let header = format!("{match_count} sessions matched '{query}'. Choose one to continue.");
    let hidden = match_count.saturating_sub(records.len());
    pick(
        &header,
        records,
        &|r| r.match_reason(query).to_string(),
        prompt,
        show_home,
        hidden,
    )
}

/// Picker for the "latest session for this project" candidates (one per
/// agent). `scope` is the scope line, e.g.
/// `Latest Claude/Codex sessions for 'web' on branch 'main'`.
pub fn choose_latest(
    scope: &str,
    records: &[SessionRecord],
    prompt: &str,
) -> io::Result<Option<usize>> {
    pick(
        scope,
        records,
        &|r| format!("latest {} session", r.agent.label()),
        prompt,
        false,
        0,
    )
}

fn pick(
    header: &str,
    records: &[SessionRecord],
    reason: &dyn Fn(&SessionRecord) -> String,
    prompt: &str,
    show_home: bool,
    hidden: usize,
) -> io::Result<Option<usize>> {
    let mut err = io::stderr().lock();
    writeln!(err, "{header}\n")?;
    for (i, r) in records.iter().enumerate() {
        writeln!(err, "{:>3}. [{}] {}", i + 1, r.agent.label(), reason(r))?;
        writeln!(err, "     {}", clean_title(r.name.as_deref()))?;
        writeln!(
            err,
            "     Project / cwd  {}",
            r.directory.as_deref().unwrap_or("Unknown")
        )?;
        writeln!(err, "     Session ID     {}", r.session_id)?;
        writeln!(err, "     Modified       {}", r.modified)?;
        if show_home {
            writeln!(err, "     Source home    {}", r.home.display())?;
        }
    }
    if hidden > 0 {
        writeln!(
            err,
            "{hidden} older matches are not shown. Use a more specific name, ID, or full path to select one."
        )?;
    }
    let stdin = io::stdin();
    loop {
        write!(err, "{prompt} (number or q to cancel): ")?;
        err.flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let choice = line.trim();
        if choice == "q" {
            return Ok(None);
        }
        match choice.parse::<usize>() {
            Ok(n) if (1..=records.len()).contains(&n) => return Ok(Some(n - 1)),
            _ => writeln!(err, "Please select one of the available options")?,
        }
    }
}

/// Every file holding `session_id` for `agent` (Claude: any
/// `projects/*/<id>.jsonl`; Codex: any `rollout-*-<id>.jsonl`; pi: any
/// `sessions/**/*_<id>.jsonl`), as distinct files (hardlinks/symlinks to
/// one file count once). opencode: the session's virtual path (one per id).
pub fn files_with_id(agent: Agent, session_id: &str, homes: &Homes) -> Vec<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let home = homes.home(agent);
    let id = session_id.to_lowercase();
    let paths = match agent {
        Agent::Claude => claude_session_paths(home, false),
        Agent::Codex => codex_session_paths(home),
        Agent::Pi => pi::session_paths(home),
        Agent::OpenCode => {
            return opencode::all_sessions(home)
                .0
                .iter()
                .filter(|r| r.id.to_lowercase() == id)
                .map(opencode::SessionRow::path)
                .collect();
        }
    };
    let mut seen = HashSet::new();
    let mut out: Vec<PathBuf> = paths
        .into_iter()
        .filter(|p| path_session_uuid(p).to_lowercase() == id)
        .filter(|p| fs::metadata(p).is_ok_and(|m| seen.insert((m.dev(), m.ino()))))
        .collect();
    out.sort();
    out
}

/// For destructive commands: when the resolved id exists in several files
/// (e.g. the same `<uuid>.jsonl` under two project dirs), pick one on a
/// TTY or fail instead of silently choosing.
fn ensure_unique_file(
    query: &str,
    res: Resolution,
    opts: &ResolveOptions,
) -> anyhow::Result<Resolution> {
    let id = res.session_id();
    let files = files_with_id(res.agent, &id, opts.homes);
    if files.len() <= 1 {
        return Ok(res);
    }
    if opts.interactive && io::stdin().is_terminal() {
        let records: Vec<SessionRecord> = files
            .iter()
            .filter_map(|p| SessionFile::read(p, res.agent, Depth::Header).ok())
            .map(|sf| {
                let mut r = SessionRecord::from_session(&sf, opts.homes.home(res.agent));
                r.matched_by = Some(MatchKind::Id);
                r
            })
            .collect();
        return match choose_record(query, &records, records.len(), opts.selection_prompt, false)? {
            Some(i) => Ok(Resolution::from_record(&records[i])),
            None => Err(Exit::fail("Session selection cancelled.").into()),
        };
    }
    let list: Vec<String> = files.iter().map(|p| format!("  {}", p.display())).collect();
    Err(Exit::fail(format!(
        "Error: Ambiguous session '{query}': session id {id} exists in {} files:\n{}\nUse the full file path.",
        files.len(),
        list.join("\n")
    ))
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    struct Fx {
        _d: tempfile::TempDir,
        homes: Homes,
        claude_a: PathBuf,
        claude_b: PathBuf,
        codex_a: PathBuf,
    }

    /// Claude A (named "alpha-work"), Claude B (named "beta"), Codex A
    /// (thread name "alpha codex"), plus an ineligible sidechain.
    fn fixture() -> Fx {
        let d = tempfile::tempdir().unwrap();
        let (ch, xh) = (d.path().join("claude"), d.path().join("codex"));
        let named = |id: &str, cwd: &str, name: &str| {
            let mut l = claude_lines(id, cwd);
            l.push(format!(
                r#"{{"type":"custom-title","customTitle":"{name}","sessionId":"{id}"}}"#
            ));
            l
        };
        let claude_a = write_claude(
            &ch,
            CLAUDE_ID,
            "/w/a",
            &named(CLAUDE_ID, "/w/a", "alpha-work"),
        );
        let claude_b = write_claude(&ch, CLAUDE_ID2, "/w/b", &named(CLAUDE_ID2, "/w/b", "beta"));
        let side_id = "c1a0de00-ffff-4abc-8def-012345678999";
        let side: Vec<String> = claude_lines(side_id, "/w/a")
            .into_iter()
            .map(|l| l.replace("\"isSidechain\":false", "\"isSidechain\":true"))
            .collect();
        let side_p = write_claude(&ch, side_id, "/w/a", &side);
        let codex_a = write_codex(
            &xh,
            CODEX_ID,
            "2026-01-02T12-57-30",
            &codex_lines(CODEX_ID, "/w/a", "cli"),
        );
        fs::write(
            xh.join("session_index.jsonl"),
            format!("{{\"id\":\"{CODEX_ID}\",\"thread_name\":\"alpha codex\"}}\n"),
        )
        .unwrap();
        set_mtime(&claude_a, 1_000);
        set_mtime(&claude_b, 2_000);
        set_mtime(&side_p, 3_000);
        set_mtime(&codex_a, 4_000);
        Fx {
            homes: Homes::new(&ch, &xh),
            claude_a,
            claude_b,
            codex_a,
            _d: d,
        }
    }

    fn single(r: &HomeResolution) -> &SessionRecord {
        match &r.outcome {
            Outcome::Single(s) => s,
            o => panic!("expected single, got {o:?}"),
        }
    }

    #[test]
    fn tier0_exact_id_includes_ineligible() {
        let fx = fixture();
        let r = resolve_in_home(
            &CLAUDE_ID.to_uppercase(),
            Agent::Claude,
            Some(&fx.homes.claude),
        )
        .unwrap();
        assert_eq!(single(&r).matched_by, Some(MatchKind::Id));
        let side = resolve_in_home(
            "c1a0de00-ffff-4abc-8def-012345678999",
            Agent::Claude,
            Some(&fx.homes.claude),
        )
        .unwrap();
        assert!(!single(&side).eligible);
        assert_eq!(
            single(&side).name,
            None,
            "ineligible Claude records are unnamed"
        );
    }

    #[test]
    fn tier1_exact_name_beats_prefix() {
        let fx = fixture();
        let r = resolve_in_home("BETA", Agent::Claude, Some(&fx.homes.claude)).unwrap();
        let s = single(&r);
        assert_eq!(
            (s.matched_by, s.session_file.clone()),
            (
                Some(MatchKind::Name),
                fs::canonicalize(&fx.claude_b).unwrap()
            )
        );
    }

    #[test]
    fn tier2_prefix_and_ambiguity() {
        let fx = fixture();
        let r = resolve_in_home("c1a0de00", Agent::Claude, Some(&fx.homes.claude)).unwrap();
        match &r.outcome {
            Outcome::Ambiguous {
                records,
                match_count,
            } => {
                assert_eq!(*match_count, 2, "sidechain excluded");
                assert_eq!(records[0].session_id, CLAUDE_ID2, "newest first");
                assert!(
                    records
                        .iter()
                        .all(|r| r.matched_by == Some(MatchKind::PartialId))
                );
            }
            o => panic!("{o:?}"),
        }
        let j = r.to_json();
        assert_eq!(j["error"], "ambiguous");
        assert_eq!(j["candidates"].as_array().unwrap().len(), 2);
        let r = resolve_in_home("c1a0de00-1234", Agent::Claude, Some(&fx.homes.claude)).unwrap();
        assert_eq!(single(&r).session_id, CLAUDE_ID);
    }

    #[test]
    fn tier3_id_substring() {
        let fx = fixture();
        let r = resolve_in_home("012345678916", Agent::Claude, Some(&fx.homes.claude)).unwrap();
        let s = single(&r);
        assert_eq!(
            (s.matched_by, s.session_id.as_str()),
            (Some(MatchKind::IdSubstring), CLAUDE_ID)
        );
    }

    #[test]
    fn tier4_filename_fragment() {
        let fx = fixture();
        let r = resolve_in_home("2026-01-02T12-57", Agent::Codex, Some(&fx.homes.codex)).unwrap();
        let s = single(&r);
        assert_eq!(s.matched_by, Some(MatchKind::Filename));
        assert_eq!(s.name.as_deref(), Some("alpha codex"));
        assert_eq!(s.directory.as_deref(), Some("/w/a"));
    }

    #[test]
    fn tier5_name_substring_and_rejections() {
        let fx = fixture();
        let r = resolve_in_home("lpha-wo", Agent::Claude, Some(&fx.homes.claude)).unwrap();
        assert_eq!(single(&r).matched_by, Some(MatchKind::Name));
        assert_eq!(
            single(&r).match_reason("lpha-wo"),
            "name/title contains query"
        );
        let r = resolve_in_home("al*", Agent::Claude, Some(&fx.homes.claude)).unwrap();
        assert_eq!(r.outcome, Outcome::NotFound);
        assert_eq!(r.to_json()["error"], "not_found");
        // Short hex never uses id tiers.
        assert_eq!(
            resolve_in_home("e28", Agent::Claude, Some(&fx.homes.claude))
                .unwrap()
                .outcome,
            Outcome::NotFound
        );
        let e = resolve_in_home("  ", Agent::Claude, Some(&fx.homes.claude)).unwrap_err();
        assert_eq!(e.code, "invalid_query");
        let e = resolve_in_home("x", Agent::Claude, Some(Path::new("/definitely/missing")))
            .unwrap_err();
        assert_eq!(e.code, "invalid_home");
    }

    #[test]
    fn pi_and_opencode_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        // Resolution canonicalizes homes (macOS: /var -> /private/var).
        let d = fs::canonicalize(tmp.path()).unwrap();
        let d = d.as_path();
        let (pi, oc) = (d.join("pi"), d.join("opencode"));
        let p = write_pi(&pi, PI_ID, "/w/api", &pi_lines(PI_ID, "/w/api"));
        let db = write_opencode_db(&oc.join("opencode.db"), "/w/api", 2_000);
        std::fs::create_dir_all(d.join("c/projects")).unwrap();
        std::fs::create_dir_all(d.join("x/sessions")).unwrap();
        let homes = Homes::new(d.join("c"), d.join("x"))
            .with_pi(&pi)
            .with_opencode(&oc);

        let r = resolve_query(PI_ID, None, &homes).unwrap();
        assert_eq!((r.agent, r.session_file.clone()), (Agent::Pi, p.clone()));
        assert_eq!(
            (r.session_id(), r.resume_target()),
            (PI_ID.to_string(), p.display().to_string())
        );
        assert_eq!(r.cwd.as_deref(), Some("/w/api"));
        let r = resolve_query(&PI_ID[..8], None, &homes).unwrap();
        assert_eq!(r.agent, Agent::Pi);

        let r = resolve_query(OPENCODE_ID, None, &homes).unwrap();
        let virt = opencode::virtual_path(&db, OPENCODE_ID);
        assert_eq!((r.agent, &r.session_file), (Agent::OpenCode, &virt));
        assert_eq!(
            (r.session_id(), r.resume_target()),
            (OPENCODE_ID.to_string(), OPENCODE_ID.to_string())
        );
        // The virtual path resolves directly; a missing id does not.
        let direct = resolve_query(&virt.to_string_lossy(), None, &homes).unwrap();
        assert_eq!(
            (direct.agent, direct.cwd.as_deref()),
            (Agent::OpenCode, Some("/w/api"))
        );
        let gone = opencode::virtual_path(&db, "ses_gone");
        assert!(matches!(
            resolve_query(&gone.to_string_lossy(), None, &homes),
            Err(QueryError::Failed(_))
        ));
        // A child session matches only by exact id.
        assert_eq!(
            resolve_query(OPENCODE_CHILD_ID, None, &homes)
                .unwrap()
                .agent,
            Agent::OpenCode
        );
        assert!(resolve_query("Subtask", None, &homes).is_err());

        // pi's session_info name and opencode's title tie on exact name.
        match resolve_query("rate limiter", None, &homes) {
            Err(QueryError::Ambiguous { records, .. }) => {
                let mut agents: Vec<Agent> = records.iter().map(|r| r.agent).collect();
                agents.sort();
                assert_eq!(agents, [Agent::Pi, Agent::OpenCode]);
            }
            o => panic!("{o:?}"),
        }
        assert_eq!(
            resolve_query("rate limiter", Some(Agent::Pi), &homes)
                .unwrap()
                .agent,
            Agent::Pi
        );

        // Absent pi / opencode homes are skipped for an all-agent search.
        let bare = Homes::new(d.join("c"), d.join("x"))
            .with_pi(d.join("no-pi"))
            .with_opencode(d.join("no-oc"));
        match resolve_query(PI_ID, None, &bare) {
            Err(QueryError::Failed(m)) => assert_eq!(m, format!("Session not found: {PI_ID}")),
            o => panic!("{o:?}"),
        }

        let one = resolve_in_home(OPENCODE_ID, Agent::OpenCode, Some(&oc)).unwrap();
        assert_eq!(one.to_json()["agent"], "opencode");
        assert_eq!(
            one.to_json()["session_file"],
            virt.to_string_lossy().as_ref()
        );
        assert_eq!(
            files_with_id(Agent::OpenCode, OPENCODE_ID, &homes),
            vec![virt]
        );
        assert_eq!(files_with_id(Agent::Pi, PI_ID, &homes), vec![p]);
        std::fs::write(oc.join("opencode-broken.db"), b"junk").unwrap();
        let err = resolve_in_home(OPENCODE_ID, Agent::OpenCode, Some(&oc)).unwrap_err();
        assert_eq!(err.code, "unreadable_database");
        assert_eq!(
            resolve_query(OPENCODE_ID, None, &homes).unwrap().agent,
            Agent::OpenCode
        );
    }

    #[test]
    fn global_query_cross_agent() {
        let fx = fixture();
        // "alpha" is a name substring in both agents -> ambiguity listing both.
        match resolve_query("alpha", None, &fx.homes) {
            Err(QueryError::Ambiguous {
                records,
                match_count,
                message,
                ..
            }) => {
                assert_eq!(match_count, 2);
                assert_eq!(records[0].agent, Agent::Codex);
                assert!(
                    message
                        .starts_with("Ambiguous session 'alpha' matches 2 sessions:\n  [codex] ")
                );
                assert!(
                    message.contains(&format!("  [claude] {CLAUDE_ID} (alpha-work)  modified "))
                );
                assert!(message.ends_with(
                    "Use a longer unique id, the exact session name, or the full file path."
                ));
            }
            o => panic!("{o:?}"),
        }
        // Exact name in Claude outranks Codex name substring.
        let r = resolve_query("alpha-work", None, &fx.homes).unwrap();
        assert_eq!(
            (r.agent, r.cwd.as_deref(), r.git_branch.as_deref()),
            (Agent::Claude, Some("/w/a"), Some("main"))
        );
        // Agent constraint.
        let r = resolve_query("alpha", Some(Agent::Codex), &fx.homes).unwrap();
        assert_eq!(r.agent, Agent::Codex);
        assert_eq!(r.session_id(), CODEX_ID);
        match resolve_query("nothing-here", None, &fx.homes) {
            Err(QueryError::Failed(m)) => assert_eq!(m, "Session not found: nothing-here"),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn global_query_direct_path() {
        let fx = fixture();
        let r = resolve_query(fx.codex_a.to_str().unwrap(), None, &fx.homes).unwrap();
        assert_eq!(
            (r.agent, r.session_file.clone()),
            (Agent::Codex, fx.codex_a.clone())
        );
        match resolve_query(fx.claude_a.to_str().unwrap(), Some(Agent::Codex), &fx.homes) {
            Err(QueryError::Failed(m)) => assert!(m.starts_with(
                "Session file is a claude session, but --agent codex was specified: "
            )),
            o => panic!("{o:?}"),
        }
        let junk = fx.homes.claude.join("junk.jsonl");
        fs::write(&junk, "nope\n").unwrap();
        match resolve_query(junk.to_str().unwrap(), None, &fx.homes) {
            Err(QueryError::Failed(m)) => {
                assert!(m.starts_with("Could not detect agent for session file: "))
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn content_id_mismatch_dropped() {
        let fx = fixture();
        // A Claude file named X whose content says Y is not resolvable by X.
        let bogus = "aaaaaaaa-0000-4abc-8def-012345678916";
        write_claude(
            &fx.homes.claude,
            bogus,
            "/w/z",
            &claude_lines(CLAUDE_ID2, "/w/z"),
        );
        assert!(matches!(
            resolve_query(bogus, None, &fx.homes),
            Err(QueryError::Failed(_))
        ));
        // But the per-home resolver (resolve command) still reports it.
        let r = resolve_in_home(bogus, Agent::Claude, Some(&fx.homes.claude)).unwrap();
        assert_eq!(single(&r).session_id, bogus);
    }

    #[test]
    fn cli_resolve_non_tty_errors() {
        let fx = fixture();
        let opts = ResolveOptions::new(&fx.homes).interactive(false);
        let e = resolve("c1a0de00", &opts).unwrap_err();
        let exit = e.downcast_ref::<Exit>().unwrap();
        assert_eq!(exit.code, 1);
        assert!(
            exit.message
                .as_deref()
                .unwrap()
                .starts_with("Error: Ambiguous session 'c1a0de00' matches 2 sessions:")
        );
        let e = resolve("zzzz-none", &opts).unwrap_err();
        assert_eq!(
            e.downcast_ref::<Exit>().unwrap().message.as_deref(),
            Some("Error: Session not found: zzzz-none")
        );
        // Exact stem fallback for trim-in-place.
        let stem_opts = ResolveOptions::new(&fx.homes)
            .agent(Some(Agent::Claude))
            .allow_claude_stem(true)
            .interactive(false);
        assert_eq!(
            exact_claude_filename(CLAUDE_ID, &fx.homes.claude),
            Some(fx.claude_a.clone())
        );
        assert!(resolve(CLAUDE_ID, &stem_opts).is_ok());
    }

    #[test]
    fn codex_state_db_titles_archived_and_fallback() {
        let fx = fixture();
        // Archived rollout outside sessions/, known only to the DB.
        let arch = write_raw(
            &fx.homes
                .codex
                .join("archived_sessions")
                .join(format!("rollout-2026-01-02T12-57-31-{CODEX_ID2}.jsonl")),
            &codex_lines(CODEX_ID2, "/w/arch", "cli"),
        );
        let db = fx.homes.codex.join("state_5.sqlite");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(&format!(
            "CREATE TABLE threads (id TEXT, rollout_path TEXT, cwd TEXT, title TEXT, archived INTEGER, git_branch TEXT);
             INSERT INTO threads VALUES ('{CODEX_ID}', '{}', '/db/cwd', 'db title one', 0, 'b1');
             INSERT INTO threads VALUES ('{CODEX_ID2}', '{}', '/w/arch', 'archived thing', 1, NULL);
             INSERT INTO threads VALUES ('stale-id', '{}', '/x', 'stale', 0, NULL);",
            fx.codex_a.display(),
            arch.display(),
            fx.codex_a.display()
        ))
        .unwrap();
        drop(c);
        // session_index name beats DB title; DB cwd beats rollout cwd.
        let r = resolve_in_home(CODEX_ID, Agent::Codex, Some(&fx.homes.codex)).unwrap();
        let s = single(&r);
        assert_eq!(
            (s.name.as_deref(), s.directory.as_deref()),
            (Some("alpha codex"), Some("/db/cwd"))
        );
        let r = resolve_in_home("archived th", Agent::Codex, Some(&fx.homes.codex)).unwrap();
        let s = single(&r);
        assert!(s.archived);
        assert_eq!(s.session_id, CODEX_ID2);
        assert_eq!(
            resolve_in_home("stale", Agent::Codex, Some(&fx.homes.codex))
                .unwrap()
                .outcome,
            Outcome::NotFound
        );

        // A broken DB is an error for the resolve command, but global
        // resolution falls back to disk.
        fs::write(&db, "not a database").unwrap();
        let e = resolve_in_home(CODEX_ID, Agent::Codex, Some(&fx.homes.codex)).unwrap_err();
        assert_eq!(e.code, "unreadable_database");
        assert_eq!(
            resolve_query(CODEX_ID, None, &fx.homes).unwrap().agent,
            Agent::Codex
        );
    }

    #[test]
    fn duplicate_ids_are_ambiguous_for_destructive_commands() {
        // Regression: the same id in two project dirs must not silently pick
        // one for a destructive command.
        let fx = fixture();
        let dup = write_claude(
            &fx.homes.claude,
            CLAUDE_ID,
            "/w/zz",
            &claude_lines(CLAUDE_ID, "/w/zz"),
        );
        let plain = ResolveOptions::new(&fx.homes).interactive(false);
        assert!(
            resolve(CLAUDE_ID, &plain).is_ok(),
            "non-destructive picks newest"
        );
        let strict = plain.clone().unique_file(true);
        let e = resolve(CLAUDE_ID, &strict).unwrap_err();
        let msg = e.downcast_ref::<Exit>().unwrap().message.clone().unwrap();
        assert!(msg.contains("exists in 2 files"), "{msg}");
        assert!(msg.contains(&dup.display().to_string()));
        // A direct path is never ambiguous.
        assert!(resolve(dup.to_str().unwrap(), &strict).is_ok());
        // A hardlink to the same file is not a duplicate.
        fs::remove_file(&dup).unwrap();
        fs::hard_link(&fx.claude_a, &dup).unwrap();
        assert!(resolve(CLAUDE_ID, &strict).is_ok());
    }

    #[test]
    fn json_shape() {
        let fx = fixture();
        let r = resolve_in_home(CODEX_ID, Agent::Codex, Some(&fx.homes.codex)).unwrap();
        let j = r.to_json();
        let keys: Vec<&String> = j.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "agent",
                "session_id",
                "name",
                "directory",
                "home",
                "session_file",
                "matched_by",
                "modified",
                "archived"
            ]
        );
        assert_eq!(j["matched_by"], "id");
        assert_eq!(j["archived"], false);
        let _ = &fx.claude_b;
    }
}
