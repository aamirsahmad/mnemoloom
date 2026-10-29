//! Session-scoped evidence of the last submitted input, read without
//! touching running sessions. Missing or ambiguous evidence stays unknown.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use mnemoloom::process;
use mnemoloom::session::Agent as SessionAgent;
use mnemoloom::session::homes::{expand_tilde, profile_homes as home_candidates};
use mnemoloom::session::meta::read_tail;
use regex::Regex;
use serde_json::{Map, Value};

use super::exec::{self, FAST};
use super::model::{Agent, Kind, State, is_working_status};
use super::procs::Snapshot;

/// Bytes of history read from the end of `history.jsonl`.
const HISTORY_LIMIT: u64 = 16 * 1024 * 1024;
/// Bytes of a Claude session registry file read.
const REGISTRY_LIMIT: u64 = 64 * 1024;
/// Synthetic "inputs" that are not something the user typed.
const SYNTHETIC: [&str; 4] = [
    "<hook_prompt",
    "<task-notification",
    "<subagent",
    "<system-reminder",
];

static ROLLOUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^rollout-.*-([0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12})\.jsonl$")
        .expect("static regex")
});

/// Last-input timestamps per session id, and how much of history was read.
type History = (HashMap<String, f64>, &'static str);

/// Open rollout paths per Codex pid; `None` when the probe failed.
pub type OpenRollouts = Option<HashMap<u32, Vec<PathBuf>>>;

/// One `lsof` (or `/proc`) probe for every Codex agent's process chain, run
/// while the panes are being captured.
pub fn probe_rollouts(snapshot: &Snapshot, codex_pids: &[u32]) -> OpenRollouts {
    let pids: Vec<u32> = codex_pids
        .iter()
        .flat_map(|&pid| snapshot.codex_pids(pid))
        .collect();
    open_rollouts(&pids)
}

/// How one agent's conversation was identified.
struct Identity {
    method: &'static str,
    /// Distinct `(home, session id)` candidates; only exactly one is trusted.
    matches: Vec<(PathBuf, String)>,
    /// The Claude registry record behind the first match.
    record: Option<Map<String, Value>>,
}

impl Identity {
    fn unique(&self) -> Option<&(PathBuf, String)> {
        match &self.matches[..] {
            [only] => Some(only),
            _ => None,
        }
    }
}

/// Add last-input evidence to `agents` in place; may promote idle -> bg.
pub fn enrich(agents: &mut [Agent], procs: &Snapshot, open: &OpenRollouts, now: f64) {
    let kinds: HashSet<Kind> = agents.iter().map(|a| a.kind).collect();
    let homes: HashMap<Kind, Vec<PathBuf>> = kinds.iter().map(|&k| (k, profile_homes(k))).collect();
    let identities: Vec<Identity> = agents
        .iter_mut()
        .map(|agent| {
            let homes = homes.get(&agent.kind).map_or(&[][..], Vec::as_slice);
            identify(agent, procs, open, homes)
        })
        .collect();
    let goals = goal_statuses(
        agents
            .iter()
            .zip(&identities)
            .filter_map(|(agent, id)| id.unique().filter(|_| agent.kind == Kind::Codex)),
    );

    let mut histories: HashMap<(PathBuf, Kind), History> = HashMap::new();
    let mut sessions: Vec<(usize, PathBuf, String)> = Vec::new();
    for (i, (agent, identity)) in agents.iter_mut().zip(&identities).enumerate() {
        let Some((home, sid)) = identity.unique() else {
            let evidence = if identity.matches.is_empty() {
                "unavailable"
            } else {
                "ambiguous"
            };
            agent.extra.input_evidence = Some(evidence.into());
            continue;
        };
        agent.session_id = sid.clone();
        match &identity.record {
            Some(record) => apply_claude_record(agent, record),
            None => {
                let goal = goals.get(home).map_or("unknown", |rows| {
                    rows.get(sid).map_or("none", String::as_str)
                });
                if goal == "active" && agent.state == State::Idle {
                    agent.state = State::Bg;
                }
                agent.extra.goal_status = Some(goal.to_owned());
            }
        }
        agent.input_scope = "session".into();
        let (times, status) = histories
            .entry((home.clone(), agent.kind))
            .or_insert_with(|| read_history(home, agent.kind, now));
        agent.last_input_at = times.get(sid).copied();
        agent.input_source = format!("{}/history-{status}", identity.method);
        agent.extra.activity_home = Some(home.to_string_lossy().into_owned());
        let evidence = if times.contains_key(sid) {
            "found"
        } else {
            "unavailable"
        };
        agent.extra.input_evidence = Some(evidence.into());
        sessions.push((i, home.clone(), sid.clone()));
    }
    mark_shared_sessions(agents, &sessions);
}

/// Reset an agent's evidence and find its candidate sessions.
fn identify(
    agent: &mut Agent,
    procs: &Snapshot,
    open: &OpenRollouts,
    homes: &[PathBuf],
) -> Identity {
    agent.session_id.clear();
    agent.last_input_at = None;
    agent.input_source = "unknown".into();
    agent.input_scope = "unknown".into();
    match agent.kind {
        Kind::Claude => {
            let start = procs.start(agent.pid).unwrap_or_default();
            let records: Vec<(PathBuf, Map<String, Value>)> = homes
                .iter()
                .filter_map(|h| claude_record(h, agent.pid, start).map(|r| (h.clone(), r)))
                .collect();
            Identity {
                method: "claude-pid",
                matches: dedupe(records.iter().map(|(h, r)| (h.clone(), session_id(r)))),
                record: records.into_iter().next().map(|(_, r)| r),
            }
        }
        Kind::Codex => {
            agent.extra.activity_probe_ok = Some(open.is_some());
            let pids = procs.codex_pids(agent.pid);
            let paths = open
                .as_ref()
                .map(|open| codex_paths(open, &pids))
                .unwrap_or_default();
            Identity {
                method: "codex-open-rollout",
                matches: rollout_matches(&paths, homes),
                record: None,
            }
        }
        // pi appends to its session file without holding it open, and
        // opencode keeps sessions in SQLite, so `lsof` names neither.
        Kind::Pi | Kind::OpenCode => Identity {
            method: "none",
            matches: Vec::new(),
            record: None,
        },
    }
}

fn dedupe(items: impl Iterator<Item = (PathBuf, String)>) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

fn mark_shared_sessions(agents: &mut [Agent], identities: &[(usize, PathBuf, String)]) {
    let mut counts: HashMap<(&PathBuf, &String), usize> = HashMap::new();
    for (_, home, sid) in identities {
        *counts.entry((home, sid)).or_default() += 1;
    }
    for (i, home, sid) in identities {
        if counts[&(home, sid)] > 1 {
            agents[*i].input_scope = "shared-session".into();
        }
    }
}

/// `~/.{kind}`, then `~/.{kind}*` profiles, then the env override; existing
/// directories only, deduplicated by resolved path.
pub fn profile_homes(kind: Kind) -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let (agent, var) = match kind {
        Kind::Claude => (SessionAgent::Claude, "CLAUDE_CONFIG_DIR"),
        Kind::Codex => (SessionAgent::Codex, "CODEX_HOME"),
        // No history or registry to read: input evidence stays unavailable.
        Kind::Pi | Kind::OpenCode => return Vec::new(),
    };
    discover_homes(&home, agent, std::env::var(var).ok().as_deref())
}

fn discover_homes(home: &Path, agent: SessionAgent, configured: Option<&str>) -> Vec<PathBuf> {
    let mut candidates = home_candidates(home, agent);
    if let Some(dir) = configured.filter(|d| !d.is_empty()) {
        candidates.push(expand_tilde(Path::new(dir)));
    }
    let mut out = Vec::new();
    for path in candidates {
        if let Ok(resolved) = fs::canonicalize(&path)
            && resolved.is_dir()
            && !out.contains(&resolved)
        {
            out.push(resolved);
        }
    }
    out
}

/// Bounded tail read of `<home>/history.jsonl`.
fn read_history(home: &Path, kind: Kind, now: f64) -> History {
    match read_tail(&home.join("history.jsonl"), HISTORY_LIMIT) {
        Ok((data, truncated)) => {
            let cut = if truncated {
                data.iter()
                    .position(|&b| b == b'\n')
                    .map_or(data.len(), |i| i + 1)
            } else {
                0
            };
            (
                parse_history(&data[cut..], kind, now),
                if truncated { "tail" } else { "complete" },
            )
        }
        Err(_) => (HashMap::new(), "unavailable"),
    }
}

fn parse_history(data: &[u8], kind: Kind, now: f64) -> HashMap<String, f64> {
    let (text_key, sid_key, ts_key, scale) = match kind {
        Kind::Claude => ("display", "sessionId", "timestamp", 1000.0),
        Kind::Codex => ("text", "session_id", "ts", 1.0),
        // Never identified (no homes), so never asked for history.
        Kind::Pi | Kind::OpenCode => return HashMap::new(),
    };
    let mut times: HashMap<String, f64> = HashMap::new();
    for line in data.split(|&b| b == b'\n') {
        let Ok(Value::Object(record)) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if let Some(Value::String(text)) = record.get(text_key)
            && SYNTHETIC.iter().any(|p| text.trim_start().starts_with(p))
        {
            continue;
        }
        let Some(Value::String(sid)) = record.get(sid_key) else {
            continue;
        };
        let Some(Value::Number(ts)) = record.get(ts_key) else {
            continue;
        };
        let Some(value) = ts.as_f64().map(|t| t / scale) else {
            continue;
        };
        if sid.is_empty() || !value.is_finite() || value <= 0.0 || value > now {
            continue;
        }
        let entry = times.entry(sid.clone()).or_insert(value);
        *entry = entry.max(value);
    }
    times
}

/// The registry record for `pid`, only if it matches the live process start.
fn claude_record(home: &Path, pid: u32, start: &str) -> Option<Map<String, Value>> {
    let file = File::open(home.join("sessions").join(format!("{pid}.json"))).ok()?;
    let mut bytes = Vec::new();
    file.take(REGISTRY_LIMIT).read_to_end(&mut bytes).ok()?;
    let Value::Object(record) = serde_json::from_slice(&bytes).ok()? else {
        return None;
    };
    validate_record(record, pid, start)
}

fn validate_record(
    record: Map<String, Value>,
    pid: u32,
    start: &str,
) -> Option<Map<String, Value>> {
    let recorded_pid = record.get("pid")?.as_u64()?;
    let Value::String(proc_start) = record.get("procStart")? else {
        return None;
    };
    let collapsed = proc_start.split_whitespace().collect::<Vec<_>>().join(" ");
    let valid = recorded_pid == u64::from(pid)
        && !start.is_empty()
        && collapsed == start
        && !session_id(&record).is_empty();
    valid.then_some(record)
}

fn session_id(record: &Map<String, Value>) -> String {
    record
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn apply_claude_record(agent: &mut Agent, record: &Map<String, Value>) {
    let status = match record.get("status") {
        None => "unknown".to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    // Claude derives names like `infra-e8` (cwd + hex) for every session;
    // only a chosen name (e.g. `/rename`) beats the descriptive pane title.
    let derived = record.get("nameSource").and_then(Value::as_str) == Some("derived");
    if let Some(Value::String(name)) = record.get("name")
        && !name.is_empty()
        && !derived
    {
        agent.name = name.clone();
    }
    if is_working_status(&status) && agent.state == State::Idle {
        agent.state = State::Bg;
    }
    agent.extra.runtime_status = Some(status);
}

/// Open rollout paths per pid. Linux reads `/proc/<pid>/fd` (lsof can block
/// on stale network mounts); elsewhere one `lsof` call. `None` means the
/// probe failed.
fn open_rollouts(pids: &[u32]) -> OpenRollouts {
    if pids.is_empty() {
        return Some(HashMap::new());
    }
    if Path::new("/proc/self/fd").is_dir() {
        return Some(proc_fds(pids));
    }
    let output = process::lsof(pids, &[]).ok();
    lsof_rollouts(output.map(|(status, out)| (status.success(), out)), pids)
}

/// lsof exits non-zero when any listed pid has gone, yet still reports the
/// live ones; only a run that could not start, timed out, or printed
/// nothing on failure counts as a failed probe.
fn lsof_rollouts(output: Option<(bool, String)>, pids: &[u32]) -> OpenRollouts {
    match output {
        Some((ok, out)) if ok || !out.is_empty() => Some(parse_lsof(&out, pids)),
        _ => None,
    }
}

fn parse_lsof(out: &str, pids: &[u32]) -> HashMap<u32, Vec<PathBuf>> {
    let mut found: HashMap<u32, Vec<PathBuf>> = HashMap::new();
    for (pid, path) in process::lsof_names(out) {
        if pids.contains(&pid) && is_rollout(Path::new(path)) {
            found.entry(pid).or_default().push(PathBuf::from(path));
        }
    }
    found
}

/// readlink every `/proc/<pid>/fd/*`; a vanished pid has no rollouts.
fn proc_fds(pids: &[u32]) -> HashMap<u32, Vec<PathBuf>> {
    pids.iter()
        .map(|&pid| {
            let links = fs::read_dir(format!("/proc/{pid}/fd"))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|fd| fs::read_link(fd.path()).ok())
                .filter(|p| is_rollout(p))
                .collect();
            (pid, links)
        })
        .collect()
}

fn is_rollout(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| ROLLOUT.is_match(&n.to_string_lossy()))
}

fn codex_paths(open: &HashMap<u32, Vec<PathBuf>>, pids: &HashSet<u32>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = pids
        .iter()
        .filter_map(|p| open.get(p))
        .flatten()
        .cloned()
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// `(home, uuid)` for each open rollout under a Codex home's `sessions/`.
fn rollout_matches(paths: &[PathBuf], homes: &[PathBuf]) -> Vec<(PathBuf, String)> {
    let pairs = paths.iter().flat_map(|path| {
        let uuid = path.file_name().and_then(|n| {
            ROLLOUT
                .captures(&n.to_string_lossy())
                .map(|c| c[1].to_owned())
        });
        homes
            .iter()
            .filter(move |home| path.starts_with(home.join("sessions")))
            .filter_map(move |home| Some((home.clone(), uuid.clone()?)))
    });
    dedupe(pairs)
}

/// Goal status rows per Codex home for the matched threads: one read-only
/// `sqlite3` call per home. A home whose query fails is absent ("unknown");
/// a thread without a row is "none".
///
/// This shells out to the `sqlite3` CLI rather than linking SQLite. Where the
/// binary is missing (minimal Linux images) the call fails silently and goal
/// status is "unknown", so an active goal does not promote idle to bg.
///
/// Session ids come from the rollout UUID regex, so they are safe to inline.
fn goal_statuses<'a>(
    matches: impl Iterator<Item = &'a (PathBuf, String)>,
) -> HashMap<PathBuf, HashMap<String, String>> {
    let mut by_home: HashMap<&PathBuf, Vec<&str>> = HashMap::new();
    for (home, sid) in matches {
        by_home.entry(home).or_default().push(sid);
    }
    by_home
        .into_iter()
        .filter_map(|(home, sids)| Some((home.clone(), goal_rows(home, &sids)?)))
        .collect()
}

fn goal_rows(home: &Path, sids: &[&str]) -> Option<HashMap<String, String>> {
    let db = home.join("goals_1.sqlite");
    if !db.is_file() {
        return None;
    }
    let list = sids
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT thread_id, status FROM thread_goals WHERE thread_id IN ({list})");
    let db = db.to_string_lossy();
    let args = [
        "-readonly",
        "-separator",
        "\t",
        "-cmd",
        ".timeout 100",
        &db,
        &sql,
    ];
    let out = exec::run("sqlite3", &args, &[], FAST)?;
    Some(parse_goal_rows(&out))
}

fn parse_goal_rows(out: &str) -> HashMap<String, String> {
    out.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(sid, status)| (sid.to_owned(), status.to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: f64 = 1_800_000_000.0;

    #[test]
    fn history_units_max_and_filters() {
        let claude = br#"{"display":"hi","timestamp":1793245773960,"sessionId":"a"}
{"display":"later","timestamp":1793245783960,"sessionId":"a"}
{"display":"  <system-reminder>x","timestamp":1799999999000,"sessionId":"a"}
{"display":"x","timestamp":true,"sessionId":"b"}
{"display":"x","timestamp":1,"sessionId":""}
{"display":"x","timestamp":1900000000000,"sessionId":"future"}
not json
[1,2]
{"display":"x","timestamp":-5,"sessionId":"neg"}"#;
        let times = parse_history(claude, Kind::Claude, NOW);
        assert_eq!(times.len(), 1);
        assert_eq!(times["a"], 1793245783.96);

        let codex = br#"{"session_id":"c","ts":1793224104,"text":"go"}
{"session_id":"c","ts":1793224000,"text":"earlier"}
{"session_id":"d","ts":1793224104,"text":"<subagent run"}"#;
        let times = parse_history(codex, Kind::Codex, NOW);
        assert_eq!(times.len(), 1);
        assert_eq!(times["c"], 1793224104.0);
    }

    #[test]
    fn history_tail_drops_partial_line() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("history.jsonl");
        let record = br#"{"session_id":"ok","ts":1,"text":"go"}"#;
        fs::write(&path, record).unwrap();
        assert_eq!(read_history(t.path(), Kind::Codex, NOW).1, "complete");
        let mut big = vec![b'x'; HISTORY_LIMIT as usize];
        big.push(b'\n');
        big.extend_from_slice(record);
        fs::write(&path, &big).unwrap();
        let (times, status) = read_history(t.path(), Kind::Codex, NOW);
        assert_eq!(status, "tail");
        assert_eq!(times, HashMap::from([("ok".into(), 1.0)]));
        assert_eq!(
            read_history(t.path().join("missing").as_path(), Kind::Codex, NOW).1,
            "unavailable"
        );
    }

    #[test]
    fn homes_follow_symlinks_and_env() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        fs::create_dir(home.join(".claude")).unwrap();
        fs::create_dir(home.join(".claude-work")).unwrap();
        fs::write(home.join(".claude.json"), "{}").unwrap();
        std::os::unix::fs::symlink(home.join(".claude"), home.join(".claude-alias")).unwrap();
        fs::create_dir(home.join("custom")).unwrap();
        let custom = home.join("custom");
        let found = discover_homes(home, SessionAgent::Claude, custom.to_str());
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names[0], ".claude");
        assert_eq!(names.len(), 3);
        assert!(names.contains(&".claude-work".to_owned()));
        assert_eq!(names[2], "custom");
    }

    fn record(pid: u64, start: &str, sid: &str) -> Map<String, Value> {
        let v = json!({"pid": pid, "procStart": start, "sessionId": sid, "status": "shell", "name": "infra-ba"});
        v.as_object().unwrap().clone()
    }

    #[test]
    fn claude_record_requires_matching_start() {
        let start = "Wed Oct 21 19:56:57 2026";
        assert!(validate_record(record(7, "Wed Oct 21  19:56:57 2026", "s"), 7, start).is_some());
        assert!(validate_record(record(7, "Wed 21 Oct 19:56:57 2026", "s"), 7, start).is_none());
        assert!(validate_record(record(8, start, "s"), 7, start).is_none());
        assert!(validate_record(record(7, start, ""), 7, start).is_none());
        assert!(validate_record(record(7, start, "s"), 7, "").is_none());
    }

    #[test]
    fn derived_registry_names_keep_the_pane_title() {
        let mut a = Agent::new("p".into(), "s".into(), Kind::Claude);
        a.name = "Managed-settings.json file location".into();
        let mut r = record(1, "x", "s");
        r.insert("nameSource".into(), json!("derived"));
        apply_claude_record(&mut a, &r);
        assert_eq!(a.name, "Managed-settings.json file location");
        r.insert("nameSource".into(), json!("user"));
        apply_claude_record(&mut a, &r);
        assert_eq!(a.name, "infra-ba");
    }

    #[test]
    fn claude_record_promotes_and_renames() {
        let mut a = Agent::new("p".into(), "s".into(), Kind::Claude);
        apply_claude_record(&mut a, &record(1, "x", "s"));
        assert_eq!(a.state, State::Bg);
        assert_eq!(a.name, "infra-ba");
        assert_eq!(a.extra.runtime_status.as_deref(), Some("shell"));
        let mut busy = Agent::new("p".into(), "s".into(), Kind::Claude);
        let mut r = record(1, "x", "s");
        r.insert("status".into(), json!("busy"));
        apply_claude_record(&mut busy, &r);
        assert_eq!(busy.state, State::Idle);
    }

    const UUID_A: &str = "01a164d8-0000-7000-8000-00000000000a";
    const UUID_B: &str = "01a164d9-0000-7000-8000-00000000000b";

    #[test]
    fn lsof_field_parsing() {
        let out = format!(
            "p100\nfcwd\nn/home/u/.codex/sessions/2026/10/22/rollout-2026-10-22T14-20-52-{UUID_A}.jsonl\n\
             n/tmp/other.jsonl\np200\nn/x/rollout-1-{UUID_B}.jsonl\np300\nn/x/rollout-2-{UUID_B}.jsonl\n"
        );
        let found = parse_lsof(&out, &[100, 200]);
        assert_eq!(found[&100].len(), 1);
        assert_eq!(found[&200].len(), 1);
        assert!(!found.contains_key(&300));
    }

    #[test]
    fn rollouts_match_homes_lexically() {
        let home = PathBuf::from("/home/u/.codex");
        let other = PathBuf::from("/home/u/.codex-alt");
        let a = home.join(format!("sessions/2026/10/22/rollout-x-{UUID_A}.jsonl"));
        let b = other.join(format!("sessions/rollout-y-{UUID_B}.jsonl"));
        let outside = PathBuf::from(format!("/tmp/rollout-z-{UUID_B}.jsonl"));
        let homes = [home.clone(), other.clone()];
        let one = rollout_matches(&[a.clone(), outside], &homes);
        assert_eq!(one, vec![(home, UUID_A.to_owned())]);
        assert_eq!(rollout_matches(&[a, b], &homes).len(), 2);
    }

    #[test]
    fn lsof_output_is_parsed_despite_exit_code() {
        let out = format!("p100\nn/x/rollout-1-{UUID_A}.jsonl\n");
        let found = lsof_rollouts(Some((false, out.clone())), &[100, 999]).unwrap();
        assert_eq!(found[&100].len(), 1);
        assert!(lsof_rollouts(Some((true, out)), &[100]).is_some());
        assert_eq!(
            lsof_rollouts(Some((true, String::new())), &[1]),
            Some(HashMap::new())
        );
        assert_eq!(lsof_rollouts(Some((false, String::new())), &[1]), None);
        assert_eq!(lsof_rollouts(None, &[1]), None);
    }

    #[test]
    fn goal_rows_parse_tab_separated() {
        let rows = parse_goal_rows(&format!("{UUID_A}\tactive\n{UUID_B}\tcomplete\nnoise\n"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[UUID_A], "active");
    }

    #[test]
    fn shared_sessions_are_marked() {
        let mut agents: Vec<Agent> = (0..3)
            .map(|i| Agent::new(format!("p{i}"), "s".into(), Kind::Claude))
            .collect();
        let h = PathBuf::from("/h");
        let ids = [
            (0, h.clone(), "x".into()),
            (1, h.clone(), "x".into()),
            (2, h, "y".into()),
        ];
        for a in &mut agents {
            a.input_scope = "session".into();
        }
        mark_shared_sessions(&mut agents, &ids);
        let scopes: Vec<&str> = agents.iter().map(|a| a.input_scope.as_str()).collect();
        assert_eq!(scopes, ["shared-session", "shared-session", "session"]);
    }
}
