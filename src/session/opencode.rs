//! opencode sessions: rows in SQLite (`<opencode home>/opencode.db` plus
//! channel DBs `opencode-<channel>.db`), always opened READ-ONLY.
//!
//! There is no per-session file, so a session is addressed by a virtual
//! path `<db path>#<session id>` ([`virtual_path`] / [`split_virtual`]).
//! [`SessionFile`]s, transcripts ([`transcript_values`]) and parent chains
//! are read from the `session` / `message` / `part` tables. A missing,
//! locked or foreign DB is skipped, never fatal.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};

use super::Agent;
use super::lineage::Derivation;
use super::message::{Message, MessageKind, Role, is_meta_text, preview_text};
use super::meta::{Depth, SessionFile};

/// How long a read waits for a writer's lock before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

/// `opencode.db` and `opencode-*.db` in `home`, sorted (empty when unset).
pub fn databases(home: &Path) -> Vec<PathBuf> {
    if home.as_os_str().is_empty() {
        return Vec::new();
    }
    let Ok(rd) = std::fs::read_dir(home) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().unwrap_or_default().to_string_lossy();
            (n == "opencode.db" || (n.starts_with("opencode-") && n.ends_with(".db")))
                && p.is_file()
        })
        .collect();
    out.sort();
    out
}

/// `<db>#<id>`.
pub fn virtual_path(db: &Path, id: &str) -> PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push("#");
    s.push(id);
    PathBuf::from(s)
}

/// Split a virtual path into `(db, id)`: the part before the last `#` must
/// end in `.db`, the id must be non-empty without `/`. No I/O.
pub fn split_virtual(path: &Path) -> Option<(PathBuf, String)> {
    let s = path.to_str()?;
    let (db, id) = s.rsplit_once('#')?;
    (db.ends_with(".db") && !id.is_empty() && !id.contains('/'))
        .then(|| (PathBuf::from(db), id.to_string()))
}

/// Open `db` read-only with a busy timeout. Never creates or writes.
pub fn open(db: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

/// One `session` row plus the counts discovery needs.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub db: PathBuf,
    pub id: String,
    pub parent_id: Option<String>,
    pub title: Option<String>,
    pub directory: Option<String>,
    /// ms since the epoch.
    pub time_created: i64,
    pub time_updated: i64,
    pub archived: bool,
    pub messages: usize,
    /// Bytes of message + part JSON.
    pub size: u64,
}

impl SessionRow {
    pub fn path(&self) -> PathBuf {
        virtual_path(&self.db, &self.id)
    }
}

const SESSION_SQL: &str = "SELECT s.id, s.parent_id, s.title, s.directory, s.time_created, \
     s.time_updated, s.time_archived, \
     (SELECT count(*) FROM message m WHERE m.session_id = s.id), \
     (SELECT coalesce(sum(length(m.data)), 0) FROM message m WHERE m.session_id = s.id) \
     + (SELECT coalesce(sum(length(p.data)), 0) FROM part p WHERE p.session_id = s.id) \
     FROM session s";

fn row(db: &Path, r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    let nonblank = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
    Ok(SessionRow {
        db: db.to_path_buf(),
        id: r.get(0)?,
        parent_id: nonblank(r.get(1)?),
        title: nonblank(r.get(2)?),
        directory: nonblank(r.get(3)?),
        time_created: r.get(4)?,
        time_updated: r.get(5)?,
        archived: r.get::<_, Option<i64>>(6)?.is_some(),
        messages: r.get::<_, i64>(7)?.max(0) as usize,
        size: r.get::<_, i64>(8)?.max(0) as u64,
    })
}

/// Every session row in one DB.
pub fn read_sessions(db: &Path) -> rusqlite::Result<Vec<SessionRow>> {
    let conn = open(db)?;
    let mut stmt = conn.prepare(SESSION_SQL)?;
    let rows = stmt.query_map([], |r| row(db, r))?;
    rows.collect()
}

/// One session row by id.
pub fn read_session(db: &Path, id: &str) -> rusqlite::Result<Option<SessionRow>> {
    let conn = open(db)?;
    conn.query_row(&format!("{SESSION_SQL} WHERE s.id = ?1"), [id], |r| {
        row(db, r)
    })
    .optional()
}

/// Sessions of every DB in `home`, deduplicated by id (newest
/// `time_updated` wins). Unreadable DBs are skipped; their errors are
/// returned alongside for callers that report them.
pub fn all_sessions(home: &Path) -> (Vec<SessionRow>, Vec<(PathBuf, rusqlite::Error)>) {
    let mut best: HashMap<String, SessionRow> = HashMap::new();
    let mut errors = Vec::new();
    for db in databases(home) {
        match read_sessions(&db) {
            Ok(rows) => {
                for r in rows {
                    if best
                        .get(&r.id)
                        .is_none_or(|b| r.time_updated > b.time_updated)
                    {
                        best.insert(r.id.clone(), r);
                    }
                }
            }
            Err(e) => errors.push((db, e)),
        }
    }
    let mut rows: Vec<SessionRow> = best.into_values().collect();
    rows.sort_by(|a, b| {
        b.time_updated
            .cmp(&a.time_updated)
            .then_with(|| a.id.cmp(&b.id))
    });
    (rows, errors)
}

fn system_time(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64)
}

/// RFC 3339 UTC with milliseconds (`2026-10-22T04:45:14.324Z`).
fn iso(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// [`SessionFile`] view of a row. `first_user_message` is read only for
/// [`Depth::Full`] (one extra query).
pub fn session_file(row: &SessionRow, depth: Depth) -> SessionFile {
    let first_user_message = match depth {
        Depth::Full => first_user_text(&row.db, &row.id).ok().flatten(),
        Depth::Header => None,
    };
    SessionFile {
        path: row.path(),
        agent: Agent::OpenCode,
        session_id: row.id.clone(),
        content_id: Some(row.id.clone()),
        cwd: row.directory.clone(),
        git_branch: None,
        name: row.title.clone(),
        created: iso(row.time_created),
        last_timestamp: iso(row.time_updated),
        modified: system_time(row.time_updated),
        changed: system_time(row.time_created),
        size: row.size,
        line_count: Some(row.messages),
        is_valid: true,
        has_conversation: row.messages > 0,
        is_sidechain: row.parent_id.is_some(),
        is_helper: false,
        is_exec_run: false,
        is_legacy_codex: false,
        parent: None,
        first_user_message,
        depth,
    }
}

fn not_found(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("no opencode session at {}", path.display()),
    )
}

/// [`SessionFile::read`] for a virtual path.
pub fn read_meta(path: &Path, depth: Depth) -> io::Result<SessionFile> {
    let (db, id) = split_virtual(path).ok_or_else(|| not_found(path))?;
    let row = read_session(&db, &id)
        .map_err(io::Error::other)?
        .ok_or_else(|| not_found(path))?;
    Ok(session_file(&row, depth))
}

/// Whether `path` names an existing opencode session.
pub fn exists(path: &Path) -> bool {
    split_virtual(path)
        .and_then(|(db, id)| read_session(&db, &id).ok().flatten())
        .is_some()
}

/// Part JSON of a session grouped by message id, in part-id order.
fn parts_by_message(conn: &Connection, id: &str) -> rusqlite::Result<HashMap<String, Vec<Value>>> {
    let mut parts: HashMap<String, Vec<Value>> = HashMap::new();
    let mut stmt =
        conn.prepare("SELECT message_id, data FROM part WHERE session_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for r in rows {
        let (mid, data) = r?;
        if let Ok(v) = serde_json::from_str(&data) {
            parts.entry(mid).or_default().push(v);
        }
    }
    Ok(parts)
}

/// One synthetic transcript "line" per message, oldest first:
/// `{"type":"message","id","role","timestamp","info":<message.data>,"parts":[<part.data>…]}`
/// (parsed by [`parse_message`]).
pub fn transcript_values(db: &Path, id: &str) -> rusqlite::Result<Vec<Value>> {
    let conn = open(db)?;
    let mut parts = parts_by_message(&conn, id)?;
    let mut stmt = conn.prepare(
        "SELECT id, time_created, data FROM message WHERE session_id = ?1 ORDER BY time_created, id",
    )?;
    let rows = stmt.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (mid, ts, data) = r?;
        let info: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
        out.push(json!({
            "type": "message",
            "role": info.get("role").cloned().unwrap_or(Value::Null),
            "timestamp": iso(ts),
            "parts": parts.remove(&mid).unwrap_or_default(),
            "id": mid,
            "info": info,
        }));
    }
    Ok(out)
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Messages of one synthetic line from [`transcript_values`].
pub fn parse_message(idx: usize, v: &Value) -> Vec<Message> {
    let role = match str_field(v, "role") {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        _ => return Vec::new(),
    };
    let ts = str_field(v, "timestamp").map(str::to_string);
    let mut out = Vec::new();
    let parts = v
        .get("parts")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for (bi, p) in parts.iter().enumerate() {
        out.extend(part(idx, bi, role, p));
    }
    for m in &mut out {
        m.timestamp.clone_from(&ts);
    }
    out
}

fn part(idx: usize, bi: usize, role: Role, p: &Value) -> Vec<Message> {
    let text = || str_field(p, "text").unwrap_or("").to_string();
    match str_field(p, "type") {
        Some("text") => {
            let mut m = Message::new(idx, bi, role, MessageKind::Text, text());
            m.is_meta = p.get("synthetic").and_then(Value::as_bool) == Some(true)
                || (role == Role::User && is_meta_text(&m.text));
            vec![m]
        }
        Some("reasoning") => vec![Message::new(idx, bi, role, MessageKind::Thinking, text())],
        Some("tool") => tool(idx, bi, p),
        Some("file") => {
            let image = str_field(p, "mime").is_some_and(|m| m.starts_with("image/"));
            let kind = if image {
                MessageKind::Image
            } else {
                MessageKind::Other
            };
            let name = str_field(p, "filename").unwrap_or("").to_string();
            vec![Message::new(idx, bi, role, kind, name)]
        }
        Some("compaction") => {
            let mut m = Message::new(
                idx,
                bi,
                Role::System,
                MessageKind::Compaction,
                String::new(),
            );
            m.is_meta = true;
            vec![m]
        }
        _ => Vec::new(),
    }
}

/// A `tool` part is both the call and (once finished) its result.
fn tool(idx: usize, bi: usize, p: &Value) -> Vec<Message> {
    let state = p.get("state").unwrap_or(&Value::Null);
    let input = state.get("input").cloned();
    let name = str_field(p, "tool").map(str::to_string);
    let id = str_field(p, "callID").map(str::to_string);
    let mut call = Message::new(
        idx,
        bi,
        Role::Assistant,
        MessageKind::ToolUse,
        input.as_ref().map(Value::to_string).unwrap_or_default(),
    );
    call.tool_name.clone_from(&name);
    call.tool_id.clone_from(&id);
    call.tool_input = input;
    let status = str_field(state, "status");
    let output = match status {
        Some("completed") => str_field(state, "output"),
        Some("error") => str_field(state, "error"),
        _ => None,
    };
    let Some(output) = output else {
        return vec![call];
    };
    let mut result = Message::new(
        idx,
        bi,
        Role::Tool,
        MessageKind::ToolResult,
        output.to_string(),
    );
    result.tool_name = name;
    result.tool_id = id;
    result.is_error = status == Some("error");
    vec![call, result]
}

/// First non-synthetic, non-meta user text part (preview form).
fn first_user_text(db: &Path, id: &str) -> rusqlite::Result<Option<String>> {
    let conn = open(db)?;
    let mut stmt = conn.prepare(
        "SELECT p.data FROM part p JOIN message m ON p.message_id = m.id \
         WHERE m.session_id = ?1 AND json_extract(m.data, '$.role') = 'user' \
         AND json_extract(p.data, '$.type') = 'text' \
         ORDER BY m.time_created, m.id, p.id",
    )?;
    let rows = stmt.query_map([id], |r| r.get::<_, String>(0))?;
    for data in rows {
        let Ok(v) = serde_json::from_str::<Value>(&data?) else {
            continue;
        };
        if v.get("synthetic").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(t) = str_field(&v, "text").filter(|t| !t.trim().is_empty() && !is_meta_text(t))
        {
            return Ok(Some(preview_text(t)));
        }
    }
    Ok(None)
}

/// `user` messages of a session (`info`).
pub fn count_user_messages(path: &Path) -> usize {
    let Some((db, id)) = split_virtual(path) else {
        return 0;
    };
    open(&db)
        .and_then(|c| {
            c.query_row(
                "SELECT count(*) FROM message WHERE session_id = ?1 \
                 AND json_extract(data, '$.role') = 'user'",
                [&id],
                |r| r.get::<_, i64>(0),
            )
        })
        .map_or(0, |n| n.max(0) as usize)
}

/// Parent chain, NEWEST first: the session and each `parent_id` ancestor
/// (same DB); every child link is [`Derivation::Subtask`], the root
/// `Original`. Cycle-safe; a missing parent ends the chain.
pub fn parent_chain(path: &Path) -> Vec<(PathBuf, Derivation)> {
    let Some((db, mut id)) = split_virtual(path) else {
        return vec![(path.to_path_buf(), Derivation::Original)];
    };
    let mut chain = Vec::new();
    let mut seen = std::collections::HashSet::new();
    while seen.insert(id.clone()) {
        let parent = read_session(&db, &id)
            .ok()
            .flatten()
            .and_then(|r| r.parent_id);
        let kind = if parent.is_some() {
            Derivation::Subtask
        } else {
            Derivation::Original
        };
        chain.push((virtual_path(&db, &id), kind));
        match parent {
            Some(p) => id = p,
            None => break,
        }
    }
    chain
}

/// Child sessions by parent virtual path (the shape of
/// [`super::lineage::child_index`], for [`super::lineage::descendants`]).
pub fn child_index(home: &Path) -> HashMap<String, Vec<(PathBuf, Derivation)>> {
    let mut map: HashMap<String, Vec<(PathBuf, Derivation)>> = HashMap::new();
    for r in all_sessions(home).0 {
        if let Some(parent) = &r.parent_id {
            map.entry(virtual_path(&r.db, parent).to_string_lossy().into_owned())
                .or_default()
                .push((r.path(), Derivation::Subtask));
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    #[test]
    fn virtual_paths_round_trip() {
        let v = virtual_path(Path::new("/d/opencode.db"), "ses_1");
        assert_eq!(v, PathBuf::from("/d/opencode.db#ses_1"));
        assert_eq!(
            split_virtual(&v),
            Some((PathBuf::from("/d/opencode.db"), "ses_1".into()))
        );
        for bad in ["/d/x.jsonl", "/d/a.db#", "/d/a.txt#ses", "/d/a.db#x/y"] {
            assert_eq!(split_virtual(Path::new(bad)), None, "{bad}");
        }
    }

    #[test]
    fn reads_sessions_and_dedupes_across_dbs() {
        let d = tempfile::tempdir().unwrap();
        let main = write_opencode_db(&d.path().join("opencode.db"), "/home/alice/src/api", 2_000);
        // Same parent id in a channel DB, older: the main DB's row wins.
        write_opencode_db(
            &d.path().join("opencode-stable.db"),
            "/home/alice/src/old",
            1_000,
        );
        std::fs::write(d.path().join("opencode-broken.db"), b"not sqlite").unwrap();
        assert_eq!(databases(d.path()).len(), 3);
        let (rows, errors) = all_sessions(d.path());
        assert_eq!(errors.len(), 1);
        assert_eq!(rows.len(), 2);
        let parent = rows.iter().find(|r| r.id == OPENCODE_ID).unwrap();
        assert_eq!(parent.db, main);
        assert_eq!(parent.directory.as_deref(), Some("/home/alice/src/api"));
        assert_eq!(parent.messages, 2);
        let sf = session_file(parent, Depth::Full);
        assert_eq!(sf.first_user_message.as_deref(), Some("add a rate limiter"));
        assert_eq!(sf.name.as_deref(), Some("Rate limiter"));
        assert!(sf.eligible() && !sf.is_sidechain);
        let child = rows.iter().find(|r| r.id == OPENCODE_CHILD_ID).unwrap();
        assert!(session_file(child, Depth::Header).is_sidechain);
        assert_eq!(
            read_meta(&parent.path(), Depth::Header).unwrap().session_id,
            OPENCODE_ID
        );
        assert!(read_meta(&virtual_path(&main, "ses_nope"), Depth::Header).is_err());
        assert!(exists(&parent.path()));
        assert_eq!(count_user_messages(&parent.path()), 1);
    }

    #[test]
    fn transcript_and_chain() {
        let d = tempfile::tempdir().unwrap();
        let db = write_opencode_db(&d.path().join("opencode.db"), "/w", 1_000);
        let values = transcript_values(&db, OPENCODE_ID).unwrap();
        let msgs: Vec<Message> = values
            .iter()
            .enumerate()
            .flat_map(|(i, v)| parse_message(i, v))
            .collect();
        let kinds: Vec<(Role, MessageKind)> = msgs.iter().map(|m| (m.role, m.kind)).collect();
        assert_eq!(
            kinds,
            [
                (Role::User, MessageKind::Text),
                (Role::User, MessageKind::Text),
                (Role::Assistant, MessageKind::Thinking),
                (Role::Assistant, MessageKind::ToolUse),
                (Role::Tool, MessageKind::ToolResult),
                (Role::Assistant, MessageKind::Text),
            ]
        );
        assert!(msgs[1].is_meta, "synthetic part");
        assert_eq!(msgs[4].text, "src\n");
        assert_eq!(
            msgs[0].timestamp.as_deref(),
            Some("1970-01-01T00:00:01.000Z")
        );
        let kids = child_index(d.path());
        assert_eq!(
            kids[&*virtual_path(&db, OPENCODE_ID).to_string_lossy()],
            vec![(virtual_path(&db, OPENCODE_CHILD_ID), Derivation::Subtask)]
        );
        let chain = parent_chain(&virtual_path(&db, OPENCODE_CHILD_ID));
        assert_eq!(
            chain,
            vec![
                (virtual_path(&db, OPENCODE_CHILD_ID), Derivation::Subtask),
                (virtual_path(&db, OPENCODE_ID), Derivation::Original),
            ]
        );
    }
}
