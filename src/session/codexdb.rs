//! Read-only access to Codex's thread database (`<codex_home>/state_<N>.sqlite`,
//! highest N wins): authoritative title / cwd / archived flag per thread.

use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};

use super::resolve::ResolverError;

/// One row of the `threads` table (only the columns used here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRow {
    pub id: String,
    pub rollout_path: Option<String>,
    pub cwd: Option<String>,
    /// Auto-captured first user message (or a user-set title).
    pub title: Option<String>,
    pub archived: bool,
    pub git_branch: Option<String>,
}

/// Highest-numbered `state_<N>.sqlite` in `home`, if any.
pub fn state_database(home: &Path) -> Option<PathBuf> {
    std::fs::read_dir(home)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n: u64 = name
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse()
                .ok()?;
            e.path().is_file().then(|| (n, e.path()))
        })
        .max_by_key(|(n, _)| *n)
        .map(|(_, p)| p)
}

fn text(v: ValueRef<'_>) -> Option<String> {
    match v {
        ValueRef::Text(b) => std::str::from_utf8(b).ok().map(str::to_string),
        _ => None,
    }
}

fn archived(v: ValueRef<'_>) -> bool {
    match v {
        ValueRef::Integer(i) => i == 1,
        ValueRef::Text(b) => matches!(
            std::str::from_utf8(b)
                .map(|s| s.trim().to_lowercase())
                .as_deref(),
            Ok("1" | "true")
        ),
        _ => false,
    }
}

/// All thread rows. Errors: `unreadable_database` (open/query failure),
/// `invalid_database` (no `id` / `rollout_path` columns).
pub fn read_threads(db: &Path) -> Result<Vec<ThreadRow>, ResolverError> {
    let unreadable = |e: rusqlite::Error, what: &str| {
        ResolverError::new(
            "unreadable_database",
            format!("Cannot {what} {}: {e}", db.display()),
        )
    };
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| unreadable(e, "open"))?;
    let mut cols = Vec::new();
    {
        let mut stmt = conn
            .prepare("PRAGMA table_info(threads)")
            .map_err(|e| unreadable(e, "query"))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(|e| unreadable(e, "query"))?;
        for c in rows {
            cols.push(c.map_err(|e| unreadable(e, "query"))?);
        }
    }
    let has = |c: &str| cols.iter().any(|x| x == c);
    let missing: Vec<&str> = ["id", "rollout_path"]
        .into_iter()
        .filter(|c| !has(c))
        .collect();
    if !missing.is_empty() {
        return Err(ResolverError::new(
            "invalid_database",
            format!(
                "Codex threads table in {} lacks: {}",
                db.display(),
                missing.join(", ")
            ),
        ));
    }
    let wanted = [
        "id",
        "rollout_path",
        "cwd",
        "title",
        "archived",
        "git_branch",
    ];
    let selected: Vec<&str> = wanted.into_iter().filter(|c| has(c)).collect();
    let idx = |c: &str| selected.iter().position(|x| *x == c);
    let sql = format!("SELECT {} FROM threads", selected.join(", "));
    let mut stmt = conn.prepare(&sql).map_err(|e| unreadable(e, "query"))?;
    let mut rows = stmt.query([]).map_err(|e| unreadable(e, "query"))?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(|e| unreadable(e, "query"))? {
        let get = |c: &str| idx(c).and_then(|i| r.get_ref(i).ok());
        let Some(id) = get("id").and_then(text).filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        out.push(ThreadRow {
            id,
            rollout_path: get("rollout_path").and_then(text),
            cwd: get("cwd").and_then(text).filter(|s| !s.is_empty()),
            title: get("title").and_then(text).filter(|s| !s.is_empty()),
            archived: get("archived").is_some_and(archived),
            git_branch: get("git_branch").and_then(text).filter(|s| !s.is_empty()),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_threads_and_picks_highest() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("state_2.sqlite"), "").unwrap();
        let db = d.path().join("state_10.sqlite");
        let c = Connection::open(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE threads (id TEXT, rollout_path TEXT, cwd TEXT, title TEXT, archived INTEGER);
             INSERT INTO threads VALUES ('a', '/r/a.jsonl', '/w', 'Title A', 1);
             INSERT INTO threads VALUES ('', '/r/x.jsonl', '/w', '', 0);
             INSERT INTO threads VALUES ('b', 'rel/b.jsonl', '', '', '0');",
        )
        .unwrap();
        drop(c);
        assert_eq!(state_database(d.path()), Some(db.clone()));
        let rows = read_threads(&db).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title.as_deref(), Some("Title A"));
        assert!(rows[0].archived && !rows[1].archived);
        assert_eq!(rows[1].cwd, None);
        assert_eq!(rows[1].git_branch, None);

        let bad = d.path().join("bad.sqlite");
        Connection::open(&bad)
            .unwrap()
            .execute_batch("CREATE TABLE threads (x TEXT);")
            .unwrap();
        assert_eq!(read_threads(&bad).unwrap_err().code, "invalid_database");
    }
}
