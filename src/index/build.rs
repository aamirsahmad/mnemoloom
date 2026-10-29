//! Incremental index update: stat every session file (and read every
//! opencode session row), re-extract only new/changed ones in parallel,
//! prune deleted ones. When nothing changed the index is not even opened
//! for writing.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;
use tantivy::schema::Term;
use tantivy::{IndexWriter, TantivyDocument};

use super::extract::{SessionDoc, extract};
use super::schema::Fields;
use super::store::{self, FileState, LockWait, Stamp, State};
use crate::session::discover::{claude_session_paths, codex_session_paths};
use crate::session::homes::normalize_path;
use crate::session::{Agent, Homes, opencode, pi};

const WRITER_BUDGET: usize = 200_000_000;
/// Below this many changed files one indexing thread suffices: the commit
/// then adds one segment, not one per thread (fewer fsyncs and merges).
const SMALL_UPDATE: usize = 16;

/// Counters reported by `build-index`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UpdateStats {
    pub indexed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub empty: usize,
    pub parse_error: usize,
    pub index_error: usize,
    pub pruned: usize,
    pub total_files: usize,
    pub claude_files: usize,
    pub codex_files: usize,
    pub pi_files: usize,
    /// opencode sessions (DB rows).
    pub opencode_sessions: usize,
    /// Notices for the user (version rebuilds).
    pub notes: Vec<String>,
    /// Another process holds the build lock (only with [`LockWait::Try`]).
    pub busy: bool,
}

impl UpdateStats {
    /// True when the index content changed.
    pub fn changed(&self) -> bool {
        self.indexed > 0 || self.pruned > 0
    }
}

/// Progress callback: `(done, total)` over the files being re-extracted.
pub type Progress<'a> = &'a (dyn Fn(usize, usize) + Sync);

/// What happened to one dirty file.
enum Outcome {
    Indexed(Box<SessionDoc>),
    /// Invalid or helper: never indexed, state marked.
    Skipped,
    /// Zero genuine user messages: state marked.
    Empty,
    /// cwd inside an agent home: state marked with the cwd.
    InHome(String),
    /// Unreadable: not marked, retried next run.
    ParseError,
}

struct Candidate {
    path: PathBuf,
    key: String,
    agent: Agent,
    stamp: Stamp,
}

/// An opencode session's stamp: `time_updated` (ms) and its data size.
fn row_stamp(row: &opencode::SessionRow) -> Stamp {
    Stamp {
        mtime_ns: (row.time_updated.max(0) as u64).saturating_mul(1_000_000),
        size: row.size,
    }
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    let md = fs::metadata(path).ok()?;
    let mtime_ns = (md.mtime().max(0) as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(md.mtime_nsec().max(0) as u64);
    Some(Stamp {
        mtime_ns,
        size: md.size(),
    })
}

/// True when `cwd` lies inside any configured (canonical) agent home.
fn cwd_in_home(cwd: &str, homes: &Homes) -> bool {
    if cwd.is_empty() {
        return false;
    }
    let cwd = normalize_path(Path::new(cwd));
    Agent::ALL
        .into_iter()
        .filter(|a| homes.has(*a))
        .any(|a| cwd.starts_with(homes.home(a)))
}

/// Whether the state entry `key` (not seen this run) is gone. A file is
/// gone when it no longer exists under that spelling; an opencode session
/// when its DB was read this run (`read_dbs`) or no longer exists, so an
/// unreadable DB, or another home's, keeps its sessions.
fn vanished(key: &str, read_dbs: &HashSet<PathBuf>) -> bool {
    match opencode::split_virtual(Path::new(key)) {
        Some((db, _)) => read_dbs.contains(&db) || !db.exists(),
        None => fs::canonicalize(key).map_or(true, |c| c.as_os_str() != key),
    }
}

fn needs_reindex(prev: Option<&FileState>, stamp: Stamp, homes: &Homes) -> bool {
    match prev {
        None => true,
        Some(p) if p.stamp != stamp => true,
        Some(p) => p
            .excluded_cwd
            .as_deref()
            .is_some_and(|c| !cwd_in_home(c, homes)),
    }
}

/// Bring the index in `dir` up to date with every home. Homes are
/// canonicalized, so every spelling of a home maps to the same keys.
/// With [`LockWait::Try`], a build running in another process makes this a
/// no-op (`busy`); the caller then reads the index as it is.
pub fn update(
    dir: &Path,
    homes: &Homes,
    progress: Option<Progress>,
    wait: LockWait,
) -> anyhow::Result<UpdateStats> {
    let homes = &homes.canonical();
    let claude = claude_session_paths(&homes.claude, true);
    let codex = codex_session_paths(&homes.codex);
    let pi = pi::session_paths(&homes.pi);
    let (rows, db_errors) = opencode::all_sessions(&homes.opencode);
    let mut stats = UpdateStats {
        claude_files: claude.len(),
        codex_files: codex.len(),
        pi_files: pi.len(),
        opencode_sessions: rows.len(),
        total_files: claude.len() + codex.len() + pi.len() + rows.len(),
        ..Default::default()
    };
    // With no sessions left, an existing index still needs pruning.
    if stats.total_files == 0 && !store::index_exists(dir) {
        return Ok(stats);
    }
    let Some(lock) = store::lock(dir, wait)? else {
        stats.busy = true;
        stats.skipped = stats.total_files;
        return Ok(stats);
    };
    if let Some(note) = store::check_version(dir, &lock)? {
        stats.notes.push(note);
    }
    let mut state = if store::index_exists(dir) {
        store::load_state(dir)
    } else {
        State::new()
    };
    // Files are stamped below; opencode rows carry their stamp.
    let files: Vec<(Agent, PathBuf, Option<Stamp>)> = claude
        .into_iter()
        .map(|p| (Agent::Claude, p, None))
        .chain(codex.into_iter().map(|p| (Agent::Codex, p, None)))
        .chain(pi.into_iter().map(|p| (Agent::Pi, p, None)))
        .chain(
            rows.iter()
                .map(|r| (Agent::OpenCode, r.path(), Some(row_stamp(r)))),
        )
        .collect();
    let seen: HashSet<String> = files
        .iter()
        .map(|(_, p, _)| p.to_string_lossy().into_owned())
        .collect();
    let dirty: Vec<Candidate> = files
        .into_par_iter()
        .filter_map(|(agent, path, stamp)| {
            let stamp = stamp.or_else(|| stamp_of(&path))?;
            let key = path.to_string_lossy().into_owned();
            needs_reindex(state.get(&key), stamp, homes).then_some(Candidate {
                path,
                key,
                agent,
                stamp,
            })
        })
        .collect();
    let read_dbs: HashSet<PathBuf> = opencode::databases(&homes.opencode)
        .into_iter()
        .filter(|db| !db_errors.iter().any(|(e, _)| e == db))
        .collect();
    // Gone, or recorded under another spelling of its path.
    let gone: Vec<String> = state
        .keys()
        .filter(|k| !seen.contains(*k) && vanished(k, &read_dbs))
        .cloned()
        .collect();
    stats.skipped = stats.total_files - dirty.len();
    if dirty.is_empty() && gone.is_empty() {
        return Ok(stats);
    }

    let (index, fields) = store::open_or_create(dir, &lock)?;
    let mut writer: IndexWriter<TantivyDocument> = if dirty.len() < SMALL_UPDATE {
        index.writer_with_num_threads(1, WRITER_BUDGET)?
    } else {
        index.writer(WRITER_BUDGET)?
    };
    let marks = Mutex::new(Vec::new());
    let counts = Mutex::new(UpdateStats::default());
    let done = AtomicUsize::new(0);
    let total = dirty.len();
    dirty.par_iter().for_each(|c| {
        let outcome = classify(c, homes);
        writer.delete_term(Term::from_field_text(fields.export_path, &c.key));
        let mut excluded_cwd = None;
        let mut mark = true;
        {
            let mut s = counts.lock().unwrap_or_else(|e| e.into_inner());
            match outcome {
                Outcome::Indexed(doc) => {
                    match writer.add_document(to_document(&doc, &fields, homes)) {
                        Ok(_) => s.indexed += 1,
                        Err(_) => {
                            s.failed += 1;
                            s.index_error += 1;
                            mark = false;
                        }
                    }
                }
                Outcome::Skipped => s.skipped += 1,
                Outcome::Empty => {
                    s.failed += 1;
                    s.empty += 1;
                }
                Outcome::InHome(cwd) => {
                    s.skipped += 1;
                    excluded_cwd = Some(cwd);
                }
                Outcome::ParseError => {
                    s.failed += 1;
                    s.parse_error += 1;
                    mark = false;
                }
            }
        }
        if mark {
            let entry = FileState {
                stamp: c.stamp,
                excluded_cwd,
            };
            marks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((c.key.clone(), entry));
        }
        if let Some(cb) = progress {
            cb(done.fetch_add(1, Ordering::Relaxed) + 1, total);
        }
    });
    for k in &gone {
        writer.delete_term(Term::from_field_text(fields.export_path, k));
        state.remove(k);
    }
    writer.commit()?;
    for c in &dirty {
        state.remove(&c.key);
    }
    state.extend(marks.into_inner().unwrap_or_else(|e| e.into_inner()));
    store::save_state(dir, &state)?;
    // Let the merges started by the commit finish so segments consolidate.
    writer.wait_merging_threads()?;
    drop(lock);

    let c = counts.into_inner().unwrap_or_else(|e| e.into_inner());
    stats.indexed = c.indexed;
    stats.skipped += c.skipped;
    stats.failed = c.failed;
    stats.empty = c.empty;
    stats.parse_error = c.parse_error;
    stats.index_error = c.index_error;
    stats.pruned = gone.len();
    Ok(stats)
}

fn classify(c: &Candidate, homes: &Homes) -> Outcome {
    let Ok(doc) = extract(&c.path, c.agent) else {
        return Outcome::ParseError;
    };
    if !doc.is_valid || doc.is_helper {
        Outcome::Skipped
    } else if doc.lines == 0 {
        Outcome::Empty
    } else if cwd_in_home(&doc.cwd, homes) {
        Outcome::InHome(doc.cwd)
    } else {
        Outcome::Indexed(Box::new(doc))
    }
}

fn to_document(d: &SessionDoc, f: &Fields, homes: &Homes) -> TantivyDocument {
    let mut doc = TantivyDocument::default();
    let bool_str = |b: bool| if b { "true" } else { "false" };
    let home = homes.home(d.agent);
    for (field, value) in [
        (f.session_id, d.session_id.as_str()),
        (f.agent, d.agent.as_str()),
        (f.project, &d.project),
        (f.branch, &d.branch),
        (f.cwd, &d.cwd),
        (f.created, &d.created),
        (f.modified, &d.modified),
        (f.export_path, &d.export_path),
        (f.first_msg_role, &d.first_msg.role),
        (f.first_msg_content, &d.first_msg.content),
        (f.last_msg_role, &d.last_msg.role),
        (f.last_msg_content, &d.last_msg.content),
        (f.first_user_msg_content, &d.first_user_msg_content),
        (f.derivation_type, &d.derivation_type),
        (f.is_sidechain, bool_str(d.is_sidechain)),
        (f.is_exec_run, bool_str(d.is_exec_run)),
        (f.claude_home, &home.to_string_lossy()),
        (f.custom_title, &d.custom_title),
        (f.content, &d.content),
    ] {
        doc.add_text(field, value);
    }
    doc.add_u64(f.modified_ts, d.modified_ts);
    doc.add_i64(f.lines, d.lines);
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    pub(crate) fn homes(root: &Path) -> Homes {
        Homes::new(root.join("claude"), root.join("codex"))
    }

    #[test]
    fn incremental_add_modify_delete() {
        let root = tempfile::tempdir().unwrap();
        let (h, idx) = (homes(root.path()), root.path().join("idx"));
        let a = write_claude(
            &h.claude,
            CLAUDE_ID,
            "/w/a",
            &claude_lines(CLAUDE_ID, "/w/a"),
        );
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.indexed, s.total_files, s.claude_files), (1, 1, 1));
        assert!(
            update(&idx, &h, None, LockWait::Block).unwrap()
                == UpdateStats {
                    skipped: 1,
                    total_files: 1,
                    claude_files: 1,
                    ..Default::default()
                }
        );

        write_codex(
            &h.codex,
            CODEX_ID,
            "2026-05-07T12-57-30",
            &codex_lines(CODEX_ID, "/w/c", "cli"),
        );
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.indexed, s.skipped), (1, 1));

        let mut lines = claude_lines(CLAUDE_ID, "/w/a");
        lines.push(lines[3].replace("fix the failing spec", "another prompt"));
        write_raw(&a, &lines);
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!(s.indexed, 1);

        fs::remove_file(&a).unwrap();
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.pruned, s.indexed), (1, 0));
        let state = store::load_state(&idx);
        assert_eq!(state.len(), 1);
        assert!(state.keys().all(|k| k.contains("rollout-")));
    }

    #[test]
    fn pi_incremental_add_modify_delete() {
        let root = tempfile::tempdir().unwrap();
        let h = homes(root.path()).with_pi(root.path().join("pi"));
        let idx = root.path().join("idx");
        let p = write_pi(&h.pi, PI_ID, "/w/pi", &pi_lines(PI_ID, "/w/pi"));
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.indexed, s.pi_files, s.total_files), (1, 1, 1));
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.indexed, s.skipped), (0, 1));
        let mut lines = pi_lines(PI_ID, "/w/pi");
        lines.push(r#"{"type":"message","id":"u4","parentId":"s1","timestamp":"2026-10-22T04:48:00.000Z","message":{"role":"user","content":"and tests"}}"#.to_string());
        write_raw(&p, &lines);
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!(s.indexed, 1);
        fs::remove_file(&p).unwrap();
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.pruned, s.indexed), (1, 0));
        assert!(store::load_state(&idx).is_empty());
    }

    #[test]
    fn opencode_incremental_by_row_stamp_and_prune() {
        let root = tempfile::tempdir().unwrap();
        let h = homes(root.path()).with_opencode(root.path().join("opencode"));
        let idx = root.path().join("idx");
        let db = write_opencode_db(&h.opencode.join("opencode.db"), "/w/oc", 1_792_644_400_000);
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.indexed, s.opencode_sessions), (2, 2));
        let keys: Vec<String> = store::load_state(&idx).into_keys().collect();
        assert!(
            keys.iter().all(|k| k.contains("opencode.db#ses_")),
            "{keys:?}"
        );
        // Unchanged rows are not re-read.
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.indexed, s.skipped), (0, 2));
        // A newer time_updated re-indexes just that session.
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE session SET time_updated = time_updated + 1 WHERE id = ?1",
            [OPENCODE_ID],
        )
        .unwrap();
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.indexed, s.skipped), (1, 1));
        // A vanished row is pruned.
        conn.execute("DELETE FROM session WHERE id = ?1", [OPENCODE_CHILD_ID])
            .unwrap();
        drop(conn);
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.pruned, s.indexed), (1, 0));
        // An unreadable DB keeps its sessions; a deleted one drops them.
        fs::write(&db, "not a database").unwrap();
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.pruned, s.opencode_sessions), (0, 0));
        assert_eq!(store::load_state(&idx).len(), 1);
        fs::remove_file(&db).unwrap();
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!(s.pruned, 1);
        assert!(store::load_state(&idx).is_empty());
    }

    #[test]
    fn exclusions() {
        let root = tempfile::tempdir().unwrap();
        let (h, idx) = (homes(root.path()), root.path().join("idx"));
        // Helper session.
        let mut helper = claude_lines(CLAUDE_ID, "/w/a");
        helper[0] = helper[0].replace(
            r#""type":"permission-mode""#,
            r#""type":"permission-mode","sessionType":"helper""#,
        );
        write_claude(&h.claude, CLAUDE_ID, "/w/a", &helper);
        // Empty: only meta user messages.
        let empty: Vec<String> = claude_lines(CLAUDE_ID2, "/w/b")
            .into_iter()
            .take(3)
            .collect();
        write_claude(&h.claude, CLAUDE_ID2, "/w/b", &empty);
        // cwd inside the Claude home.
        let inside = h.claude.join("plugins").to_string_lossy().into_owned();
        let id3 = "c1a0de00-1111-4abc-8def-012345678918";
        write_claude(&h.claude, id3, &inside, &claude_lines(id3, &inside));
        // Invalid file.
        write_raw(
            &h.claude.join("projects/x/bogus.jsonl"),
            &["{\"a\":1}".to_string()],
        );
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.indexed, s.empty, s.skipped), (0, 1, 3));
        let state = store::load_state(&idx);
        assert_eq!(state.len(), 4);
        assert_eq!(
            state.values().filter(|e| e.excluded_cwd.is_some()).count(),
            1
        );
        // Unchanged: nothing re-read.
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!((s.indexed, s.skipped), (0, 4));
        // Different Claude home: that cwd is no longer inside a home.
        let other = Homes::new(root.path().join("other"), h.codex.clone());
        write_claude(&other.claude, id3, &inside, &claude_lines(id3, &inside));
        let s = update(&idx, &other, None, LockWait::Block).unwrap();
        assert_eq!(s.indexed, 1);
    }

    #[test]
    fn subagents_and_exec_runs_are_indexed() {
        let root = tempfile::tempdir().unwrap();
        let (h, idx) = (homes(root.path()), root.path().join("idx"));
        let p = write_claude(
            &h.claude,
            CLAUDE_ID,
            "/w/a",
            &claude_lines(CLAUDE_ID, "/w/a"),
        );
        let sub = p
            .with_extension("")
            .join("subagents")
            .join("agent-abc.jsonl");
        write_raw(&sub, &claude_lines(CLAUDE_ID, "/w/a"));
        write_codex(
            &h.codex,
            CODEX_ID,
            "2026-05-07T12-57-30",
            &codex_lines(CODEX_ID, "/w/c", "exec"),
        );
        let s = update(&idx, &h, None, LockWait::Block).unwrap();
        assert_eq!(s.indexed, 3);
    }
}
