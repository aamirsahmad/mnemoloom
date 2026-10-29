//! Synthetic Claude / Codex / pi / opencode fixtures for unit tests (shapes
//! match real transcripts; paths are synthetic).

use std::fs;
use std::path::{Path, PathBuf};

use super::encode::encode_project_path;

pub const CLAUDE_ID: &str = "c1a0de00-1234-4abc-8def-012345678916";
pub const CLAUDE_ID2: &str = "c1a0de00-0000-4abc-8def-012345678917";
pub const CODEX_ID: &str = "019c0dec-0001-7000-8000-0000000000c5";
pub const CODEX_ID2: &str = "019c0dec-aaaa-7000-8000-0000000000c6";

/// A realistic Claude transcript: meta user line, real prompt, tool round
/// trip, `custom-title` then a later `agent-name` ("renamed-later").
pub fn claude_lines(id: &str, cwd: &str) -> Vec<String> {
    let env = format!(
        r#""isSidechain":false,"userType":"external","entrypoint":"cli","cwd":"{cwd}","sessionId":"{id}","version":"2.1.311","gitBranch":"main""#
    );
    vec![
        format!(r#"{{"type":"permission-mode","permissionMode":"bypassPermissions","sessionId":"{id}"}}"#),
        r#"{"type":"file-history-snapshot","messageId":"1001","snapshot":{"messageId":"1001","trackedFileBackups":{},"timestamp":"2026-01-03T03:28:00.000Z"},"isSnapshotUpdate":false}"#.to_string(),
        format!(r#"{{"parentUuid":null,{env},"type":"user","isMeta":true,"message":{{"role":"user","content":"<command-name>/clear</command-name>"}},"uuid":"u0","timestamp":"2026-01-03T03:28:25.917Z"}}"#),
        format!(r#"{{"parentUuid":"u0",{env},"type":"user","message":{{"role":"user","content":"fix the failing spec in user_spec.rb"}},"uuid":"u1","timestamp":"2026-01-03T03:28:26.000Z"}}"#),
        format!(r#"{{"type":"custom-title","customTitle":"first-name","sessionId":"{id}"}}"#),
        format!(r#"{{"parentUuid":"u1",{env},"message":{{"model":"claude-opus-5-5","id":"msg_1","type":"message","role":"assistant","content":[{{"type":"tool_use","id":"toolu_1","name":"Bash","input":{{"command":"bundle exec rspec","description":"Run spec"}}}}],"stop_reason":"tool_use","usage":{{"input_tokens":2,"output_tokens":181}}}},"type":"assistant","uuid":"a1","timestamp":"2026-01-03T03:28:30.000Z"}}"#),
        format!(r#"{{"parentUuid":"a1",{env},"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"toolu_1","type":"tool_result","content":"F.\nFailures:","is_error":false}}]}},"uuid":"u2","toolUseResult":{{"stdout":"F.","stderr":"","interrupted":false}},"timestamp":"2026-01-03T03:28:40.000Z"}}"#),
        format!(r#"{{"type":"agent-name","agentName":"renamed-later","sessionId":"{id}"}}"#),
        format!(r#"{{"parentUuid":"u2",{env},"message":{{"role":"assistant","content":[{{"type":"text","text":"Fixed."}}]}},"type":"assistant","uuid":"a2","timestamp":"2026-01-03T03:30:00.000Z"}}"#),
        format!(r#"{{"type":"last-prompt","leafUuid":"a2","sessionId":"{id}"}}"#),
    ]
}

/// A realistic Codex rollout. `source` is a JSON string value (e.g. `cli`,
/// `exec`) or, when it starts with `{`, a raw JSON object.
pub fn codex_lines(id: &str, cwd: &str, source: &str) -> Vec<String> {
    let source = if source.starts_with('{') {
        source.to_string()
    } else {
        format!("\"{source}\"")
    };
    vec![
        format!(r#"{{"timestamp":"2026-01-02T16:58:02.928Z","type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-01-02T16:57:30.619Z","cwd":"{cwd}","originator":"codex-tui","cli_version":"0.1.0","source":{source},"model_provider":"openai","git":{{"commit_hash":"0000","branch":"fix/timeout","repository_url":"git@github.com:example/app.git"}},"base_instructions":"long"}}}}"#),
        format!(r#"{{"timestamp":"2026-01-02T16:58:03.000Z","type":"turn_context","payload":{{"turn_id":"t1","cwd":"{cwd}","approval_policy":"on-request","sandbox_policy":{{"type":"workspace-write"}},"model":"gpt-5.4"}}}}"#),
        r#"{"timestamp":"2026-01-02T16:58:03.100Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>x"}]}}"#.to_string(),
        r#"{"timestamp":"2026-01-02T16:58:03.200Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/w</cwd>\n</environment_context>"}]}}"#.to_string(),
        r#"{"timestamp":"2026-01-02T16:58:04.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello codex"}]}}"#.to_string(),
        r#"{"timestamp":"2026-01-02T16:58:04.001Z","type":"event_msg","payload":{"type":"user_message","message":"hello codex","images":[],"local_images":[],"text_elements":[]}}"#.to_string(),
        r#"{"timestamp":"2026-01-02T16:58:05.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"ls\"}","call_id":"call_1"}}"#.to_string(),
        r#"{"timestamp":"2026-01-02T16:58:06.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"a\nb"}}"#.to_string(),
        r#"{"timestamp":"2026-01-02T16:58:07.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}],"phase":"final_answer"}}"#.to_string(),
    ]
}

pub fn write_raw(path: &Path, lines: &[String]) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, lines.join("\n") + "\n").unwrap();
    path.to_path_buf()
}

/// `<home>/projects/<encode(cwd)>/<id>.jsonl`.
pub fn write_claude(home: &Path, id: &str, cwd: &str, lines: &[String]) -> PathBuf {
    write_raw(
        &home
            .join("projects")
            .join(encode_project_path(cwd))
            .join(format!("{id}.jsonl")),
        lines,
    )
}

/// `<home>/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl` (`ts` like `2026-01-02T12-57-30`).
pub fn write_codex(home: &Path, id: &str, ts: &str, lines: &[String]) -> PathBuf {
    let (y, m, d) = (&ts[0..4], &ts[5..7], &ts[8..10]);
    write_raw(
        &home
            .join("sessions")
            .join(y)
            .join(m)
            .join(d)
            .join(format!("rollout-{ts}-{id}.jsonl")),
        lines,
    )
}

/// Set a file's mtime to `secs` after the epoch (ordering tests).
pub fn set_mtime(path: &Path, secs: u64) {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

pub const PI_ID: &str = "019f0e1d-0001-7000-8000-0000000000a1";
pub const OPENCODE_ID: &str = "ses_0a1b2c3d4e5fParentAAAAAAAA";
pub const OPENCODE_CHILD_ID: &str = "ses_0a1b2c3d4e5fChildBBBBBBBB";

/// A pi session with a branch: after `a1b` the user first asked `u2`
/// ("try redis", abandoned), then edited it into `u3`; the active branch
/// (last entry `s1` back to the root) skips `u2`/`a2`.
pub fn pi_lines(id: &str, cwd: &str) -> Vec<String> {
    let msg = |eid: &str, parent: &str, ts: &str, message: &str| {
        format!(
            r#"{{"type":"message","id":"{eid}","parentId":"{parent}","timestamp":"2026-10-22T04:{ts}Z","message":{message}}}"#
        )
    };
    vec![
        format!(r#"{{"type":"session","version":3,"id":"{id}","timestamp":"2026-10-22T04:45:14.324Z","cwd":"{cwd}"}}"#),
        r#"{"type":"model_change","id":"m0","parentId":null,"timestamp":"2026-10-22T04:45:14.400Z","provider":"anthropic","modelId":"claude-opus-5-5"}"#.to_string(),
        msg("sys", "m0", "45:14.500", r#"{"role":"system","content":"You are pi.","timestamp":1792644314500}"#),
        msg("u1", "sys", "45:15.000", r#"{"role":"user","content":[{"type":"text","text":"add a rate limiter"}],"timestamp":1792644315000}"#),
        msg("a1", "u1", "45:20.000", r#"{"role":"assistant","content":[{"type":"thinking","thinking":"plan"},{"type":"toolCall","id":"call_1","name":"bash","arguments":{"command":"ls"}}],"stopReason":"toolUse"}"#),
        msg("t1", "a1", "45:21.000", r#"{"role":"toolResult","toolCallId":"call_1","toolName":"bash","content":[{"type":"text","text":"src\n"}],"isError":false}"#),
        msg("a1b", "t1", "45:25.000", r#"{"role":"assistant","content":[{"type":"text","text":"Here is the layout."}],"stopReason":"stop"}"#),
        msg("u2", "a1b", "46:00.000", r#"{"role":"user","content":"try redis"}"#),
        msg("a2", "u2", "46:05.000", r#"{"role":"assistant","content":[{"type":"text","text":"Using redis."}]}"#),
        msg("u3", "a1b", "46:30.000", r#"{"role":"user","content":"use a token bucket"}"#),
        msg("a3", "u3", "46:40.000", r#"{"role":"assistant","content":[{"type":"text","text":"Token bucket added."}]}"#),
        r#"{"type":"session_info","id":"s1","parentId":"a3","timestamp":"2026-10-22T04:47:00.000Z","name":"rate limiter"}"#.to_string(),
    ]
}

/// `<home>/sessions/<encoded cwd>/2026-10-22T04-45-14-324Z_<id>.jsonl`.
pub fn write_pi(home: &Path, id: &str, cwd: &str, lines: &[String]) -> PathBuf {
    let dir = format!("-{}-", cwd.replace('/', "-"));
    write_raw(
        &home
            .join("sessions")
            .join(dir)
            .join(format!("2026-10-22T04-45-14-324Z_{id}.jsonl")),
        lines,
    )
}

/// Create an opencode DB at `db` (real table / column names) holding
/// [`OPENCODE_ID`] (in `directory`, title "Rate limiter", updated at
/// `updated` ms) and its child [`OPENCODE_CHILD_ID`].
pub fn write_opencode_db(db: &Path, directory: &str, updated: i64) -> PathBuf {
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "CREATE TABLE project (id text PRIMARY KEY, worktree text NOT NULL, vcs text, name text,
             time_created integer NOT NULL, time_updated integer NOT NULL, sandboxes text NOT NULL);
         CREATE TABLE session (id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
             slug text NOT NULL, directory text NOT NULL, title text NOT NULL, version text NOT NULL,
             time_created integer NOT NULL, time_updated integer NOT NULL, time_compacting integer,
             time_archived integer, agent text, model text);
         CREATE TABLE message (id text PRIMARY KEY, session_id text NOT NULL,
             time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL);
         CREATE TABLE part (id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL,
             time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project VALUES ('prj_1', ?1, 'git', 'api', 0, 0, '[]')",
        [directory],
    )
    .unwrap();
    let session = "INSERT INTO session (id, project_id, parent_id, slug, directory, title, version,
         time_created, time_updated) VALUES (?1, 'prj_1', ?2, 'slug', ?3, ?4, '1.18.0', 500, ?5)";
    conn.execute(
        session,
        rusqlite::params![
            OPENCODE_ID,
            None::<String>,
            directory,
            "Rate limiter",
            updated
        ],
    )
    .unwrap();
    conn.execute(
        session,
        rusqlite::params![
            OPENCODE_CHILD_ID,
            OPENCODE_ID,
            directory,
            "Subtask",
            updated - 1
        ],
    )
    .unwrap();
    let messages = [
        (
            "msg_1",
            OPENCODE_ID,
            1000,
            r#"{"role":"user","time":{"created":1000},"agent":"build"}"#,
        ),
        (
            "msg_2",
            OPENCODE_ID,
            2000,
            r#"{"role":"assistant","time":{"created":2000},"modelID":"m","providerID":"p","mode":"build","path":{"cwd":"/w","root":"/w"}}"#,
        ),
        (
            "msg_3",
            OPENCODE_CHILD_ID,
            3000,
            r#"{"role":"user","time":{"created":3000}}"#,
        ),
    ];
    for (id, sid, t, data) in messages {
        conn.execute(
            "INSERT INTO message VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![id, sid, t, data],
        )
        .unwrap();
    }
    let parts = [
        (
            "prt_1",
            "msg_1",
            OPENCODE_ID,
            r#"{"type":"text","text":"add a rate limiter"}"#,
        ),
        (
            "prt_2",
            "msg_1",
            OPENCODE_ID,
            r#"{"type":"text","text":"Called the Read tool","synthetic":true}"#,
        ),
        (
            "prt_3",
            "msg_2",
            OPENCODE_ID,
            r#"{"type":"step-start","snapshot":"abc"}"#,
        ),
        (
            "prt_4",
            "msg_2",
            OPENCODE_ID,
            r#"{"type":"reasoning","text":"plan","time":{"start":1,"end":2}}"#,
        ),
        (
            "prt_5",
            "msg_2",
            OPENCODE_ID,
            r#"{"type":"tool","tool":"bash","callID":"call_1","state":{"status":"completed","input":{"command":"ls"},"output":"src\n","title":"ls","time":{"start":1,"end":2}}}"#,
        ),
        (
            "prt_6",
            "msg_2",
            OPENCODE_ID,
            r#"{"type":"text","text":"Done."}"#,
        ),
        (
            "prt_7",
            "msg_2",
            OPENCODE_ID,
            r#"{"type":"step-finish","reason":"stop"}"#,
        ),
        (
            "prt_8",
            "msg_3",
            OPENCODE_CHILD_ID,
            r#"{"type":"text","text":"explore the code"}"#,
        ),
    ];
    for (id, mid, sid, data) in parts {
        conn.execute(
            "INSERT INTO part VALUES (?1, ?2, ?3, 0, 0, ?4)",
            rusqlite::params![id, mid, sid, data],
        )
        .unwrap();
    }
    db.to_path_buf()
}
