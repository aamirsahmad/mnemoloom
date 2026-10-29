//! Session lineage: parent pointers written on the FIRST line of derived
//! sessions (`trim_metadata.parent_file`, `continue_metadata.parent_session_file`),
//! chain walking, descendant search, and the `[SESSION LINEAGE]` texts.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde_json::Value;

use super::Agent;
use super::timefmt::parse_iso;

/// How a session was derived from its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Derivation {
    /// No parent (or the chain's root).
    Original,
    /// `trim_metadata` (trim / smart-trim).
    Trimmed,
    /// `continue_metadata` (rollover / port).
    Continued,
    /// opencode child session (`session.parent_id`); only in
    /// [`full_chain`] of an opencode session.
    Subtask,
}

impl Derivation {
    /// Wire name: `original` / `trimmed` / `continued`.
    pub fn as_str(self) -> &'static str {
        match self {
            Derivation::Original => "original",
            Derivation::Trimmed => "trimmed",
            Derivation::Continued => "continued",
            Derivation::Subtask => "subtask",
        }
    }

    /// Rollover-prompt label (`continued` -> `rollover`).
    pub fn friendly(self) -> &'static str {
        match self {
            Derivation::Continued => "rollover",
            other => other.as_str(),
        }
    }

    /// Trim-injection label (`continued` -> `rolled over`).
    pub fn injection_label(self) -> &'static str {
        match self {
            Derivation::Continued => "rolled over",
            other => other.as_str(),
        }
    }
}

/// Parent pointer read from a session's first line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentLink {
    /// [`Derivation::Trimmed`] or [`Derivation::Continued`].
    pub kind: Derivation,
    /// `trim_metadata.parent_file` / `continue_metadata.parent_session_file`.
    pub parent_file: PathBuf,
    /// `continue_metadata.parent_session_id`, when present.
    pub parent_session_id: Option<String>,
    /// `continue_metadata.exported_chat_log`, when present (legacy).
    pub exported_chat_log: Option<PathBuf>,
    /// `continue_metadata.ported_from` (`"claude"` / `"codex"`).
    pub ported_from: Option<String>,
}

/// Parse the parent pointer from a first-line JSON value.
/// `continue_metadata` (with a non-empty `parent_session_file`) wins over
/// `trim_metadata.parent_file`.
pub fn parent_from_value(v: &Value) -> Option<ParentLink> {
    let nonempty = |m: &Value, k: &str| {
        m.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if let Some(cm) = v.get("continue_metadata").filter(|m| m.is_object())
        && let Some(parent) = nonempty(cm, "parent_session_file")
    {
        return Some(ParentLink {
            kind: Derivation::Continued,
            parent_file: PathBuf::from(parent),
            parent_session_id: nonempty(cm, "parent_session_id"),
            exported_chat_log: nonempty(cm, "exported_chat_log").map(PathBuf::from),
            ported_from: nonempty(cm, "ported_from"),
        });
    }
    let tm = v.get("trim_metadata").filter(|m| m.is_object())?;
    Some(ParentLink {
        kind: Derivation::Trimmed,
        parent_file: PathBuf::from(nonempty(tm, "parent_file")?),
        parent_session_id: None,
        exported_chat_log: None,
        ported_from: None,
    })
}

/// Read only the first physical line of `path` and parse it as JSON.
pub fn read_first_line(path: &Path) -> Option<Value> {
    let mut reader = BufReader::new(File::open(path).ok()?);
    let mut buf = Vec::new();
    reader.read_until(b'\n', &mut buf).ok()?;
    serde_json::from_slice(buf.trim_ascii()).ok()
}

/// Parent pointer of `path` (first line only). `None` for originals and
/// missing/unreadable files.
pub fn parent_info(path: &Path) -> Option<ParentLink> {
    parent_from_value(&read_first_line(path)?)
}

/// Full chain, NEWEST first, ending with the root typed `Original`.
/// A non-derived file yields `[(file, Original)]`. A missing parent is
/// appended as `Original`. Cycle-safe. An opencode virtual path follows
/// `parent_id` instead ([`super::opencode::parent_chain`]).
pub fn full_chain(path: &Path) -> Vec<(PathBuf, Derivation)> {
    if super::opencode::split_virtual(path).is_some() {
        return super::opencode::parent_chain(path);
    }
    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    let mut current = path.to_path_buf();
    loop {
        if !visited.insert(current.clone()) {
            break;
        }
        match parent_info(&current) {
            Some(link) => {
                chain.push((current, link.kind));
                current = link.parent_file;
            }
            None => {
                chain.push((current, Derivation::Original));
                break;
            }
        }
    }
    chain
}

/// Continued (rollover) members of the chain, OLDEST first — the files
/// that themselves carry `continue_metadata`.
pub fn continuation_lineage(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    let mut current = path.to_path_buf();
    while visited.insert(current.clone()) {
        let Some(link) = parent_info(&current) else {
            break;
        };
        if link.kind == Derivation::Continued {
            out.push(current.clone());
        }
        current = link.parent_file;
    }
    out.reverse();
    out
}

/// Every `*.jsonl` under `dirs` (recursive), first-line parent pointers
/// read in parallel: `parent_file string -> children`. Children are matched
/// on both `trim_metadata` and `continue_metadata` (so find-derived
/// also sees rollover children).
pub fn child_index(dirs: &[PathBuf]) -> HashMap<String, Vec<(PathBuf, Derivation)>> {
    let mut files = Vec::new();
    for d in dirs {
        collect_jsonl(d, &mut files);
    }
    let links: Vec<(PathBuf, ParentLink)> = files
        .into_par_iter()
        .filter_map(|f| parent_info(&f).map(|l| (f, l)))
        .collect();
    let mut map: HashMap<String, Vec<(PathBuf, Derivation)>> = HashMap::new();
    for (f, l) in links {
        map.entry(l.parent_file.to_string_lossy().into_owned())
            .or_default()
            .push((f, l.kind));
    }
    map
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut visited = HashSet::new();
    super::discover::walk_jsonl(dir, out, &mut visited, &|p| {
        p.extension().is_some_and(|x| x == "jsonl")
    });
}

/// BFS from `root` over `index` (see [`child_index`]); returns
/// `node -> direct children` for every visited node. A node matches parent
/// strings equal to its absolute or its canonical path.
pub fn descendants(
    root: &Path,
    index: &HashMap<String, Vec<(PathBuf, Derivation)>>,
) -> Vec<(PathBuf, Vec<(PathBuf, Derivation)>)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    while let Some(cur) = queue.pop_front() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        let abs = super::absolute(&cur);
        let canon = std::fs::canonicalize(&cur).unwrap_or_else(|_| abs.clone());
        let mut kids: Vec<(PathBuf, Derivation)> = [abs, canon]
            .iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .filter_map(|k| index.get(k.to_string_lossy().as_ref()))
            .flatten()
            .cloned()
            .collect();
        kids.sort();
        kids.dedup();
        queue.extend(kids.iter().map(|(p, _)| p.clone()));
        out.push((cur, kids));
    }
    out
}

/// The note prepended to the first user message of a trimmed (or
/// rolled-over) session. `chain_newest_first` is the INPUT file's
/// [`full_chain`]; returns `None` when it has a single element (nothing
/// is injected for first-generation derivations).
pub fn injection_note(
    chain_newest_first: &[(PathBuf, Derivation)],
    current: Derivation,
) -> Option<String> {
    if chain_newest_first.len() <= 1 {
        return None;
    }
    let file_list = chain_newest_first
        .iter()
        .rev()
        .enumerate()
        .map(|(i, (path, d))| match d {
            Derivation::Original => format!("  {}. {} (original)", i + 1, path.display()),
            d => format!(
                "  {}. {} ({} from {})",
                i + 1,
                path.display(),
                d.injection_label(),
                i
            ),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let explanation = if current == Derivation::Continued {
        "This session was ROLLED OVER from a parent session. Work was handed off to this fresh \
         session with a summary of the task. The parent session is fully preserved and you can \
         read it for detailed context if needed."
    } else {
        "This session was TRIMMED from a parent session. Long messages (tool results and \
         assistant responses) were truncated to free up context space. Each truncated message \
         shows a marker like:\n  [... See line N of /path/to/parent.jsonl for full content]\n\
         You can read the parent file at that line number to retrieve the full original content."
    };
    Some(format!(
        "\n\n[SESSION LINEAGE]\n\n{explanation}\n\n\
         Parent sessions in chronological order (oldest to newest):\n{file_list}\n\n\
         Use sub-agents to read these files if you need more context.\n\n"
    ))
}

/// Prepend `note` to the first user message of a transcript line set
/// Returns the index of the modified line.
pub fn inject_into_first_user_message(
    agent: Agent,
    lines: &mut [super::transcript::Line],
    note: &str,
) -> Option<usize> {
    for (i, line) in lines.iter_mut().enumerate() {
        let Some(v) = line.value() else { continue };
        let target = match agent {
            Agent::Claude => v.get("type").and_then(Value::as_str) == Some("user"),
            Agent::Codex => {
                v.get("type").and_then(Value::as_str) == Some("input")
                    || (v.get("type").and_then(Value::as_str) == Some("response_item")
                        && v.pointer("/payload/type").and_then(Value::as_str) == Some("message")
                        && v.pointer("/payload/role").and_then(Value::as_str) == Some("user"))
            }
            // Never rewritten (see `Agent::require_rewritable`).
            Agent::Pi | Agent::OpenCode => return None,
        };
        if !target {
            continue;
        }
        let mut v = v.clone();
        if prepend_note(agent, &mut v, note) {
            line.set_value(v);
            return Some(i);
        }
    }
    None
}

fn prepend_note(agent: Agent, v: &mut Value, note: &str) -> bool {
    let prepend = |s: &mut Value| {
        let old = s.as_str().unwrap_or("").to_string();
        *s = Value::String(format!("{note}{old}"));
    };
    match agent {
        Agent::Claude => {
            let Some(msg) = v.get_mut("message") else {
                return false;
            };
            match msg.get_mut("content") {
                Some(Value::Array(items)) if !items.is_empty() => {
                    if let Some(item) = items
                        .iter_mut()
                        .find(|i| i.get("type").and_then(Value::as_str) == Some("text"))
                    {
                        let text = item.get_mut("text");
                        match text {
                            Some(t) => prepend(t),
                            None => item["text"] = Value::String(note.to_string()),
                        }
                    } else {
                        items.insert(0, serde_json::json!({"type": "text", "text": note}));
                    }
                    true
                }
                Some(s @ Value::String(_)) => {
                    prepend(s);
                    true
                }
                _ => false,
            }
        }
        Agent::Codex => {
            if v.get("type").and_then(Value::as_str) == Some("input") {
                return match v.get_mut("content") {
                    Some(s @ Value::String(_)) => {
                        prepend(s);
                        true
                    }
                    _ => false,
                };
            }
            let Some(Value::Array(items)) = v.pointer_mut("/payload/content") else {
                return false;
            };
            match items.iter_mut().find(|i| i.get("text").is_some()) {
                Some(item) => {
                    prepend(item.get_mut("text").expect("checked"));
                    true
                }
                None => false,
            }
        }
        Agent::Pi | Agent::OpenCode => false,
    }
}

/// First and last `timestamp` / `isoTimestamp` / `snapshot.timestamp` in a
/// file (rollover file-list span). Falls back to mtime for `modified`.
pub fn session_time_bounds(path: &Path) -> (Option<String>, Option<String>) {
    let mut created = None;
    let mut modified = None;
    if let Ok(f) = File::open(path) {
        for line in BufReader::new(f).split(b'\n').map_while(Result::ok) {
            let Ok(v) = serde_json::from_slice::<Value>(line.trim_ascii()) else {
                continue;
            };
            let ts = ["timestamp", "isoTimestamp"]
                .iter()
                .find_map(|k| v.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))
                .or_else(|| {
                    v.pointer("/snapshot/timestamp")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                });
            if let Some(ts) = ts {
                if created.is_none() {
                    created = Some(ts.to_string());
                }
                modified = Some(ts.to_string());
            }
        }
    }
    if modified.is_none()
        && let Ok(m) = std::fs::metadata(path).and_then(|m| m.modified())
    {
        let dt: chrono::DateTime<chrono::Local> = m.into();
        modified = Some(dt.format("%Y-%m-%dT%H:%M:%S").to_string());
    }
    (created, modified)
}

/// `"{span}, {YYYY-MM-DD HH:MM local}"` (span `Nd`/`Nh`/`Nm`/`<1m`), or
/// just the modified display when the span can't be computed.
pub fn format_time_span(created: Option<&str>, modified: Option<&str>) -> String {
    let Some(modified) = modified else {
        return String::new();
    };
    let mod_dt = parse_iso(modified);
    let mod_display = match mod_dt {
        Some(d) => d
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => modified.chars().take(16).collect(),
    };
    let span = created.and_then(parse_iso).zip(mod_dt).map(|(c, m)| {
        let secs = (m - c).num_seconds();
        let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
        let (hours, minutes_total) = (rem / 3600, secs.div_euclid(60));
        if days >= 1 {
            format!("{days}d")
        } else if hours >= 1 {
            format!("{hours}h")
        } else if minutes_total >= 1 {
            format!("{minutes_total}m")
        } else {
            "<1m".to_string()
        }
    });
    match span {
        Some(s) => format!("{s}, {mod_display}"),
        None => mod_display,
    }
}

/// Numbered rollover file list (chronological input), e.g.
/// `1. /a.jsonl (original, 2d, 2026-10-01 10:00)`.
pub fn session_file_list(chronological: &[(PathBuf, Derivation)]) -> String {
    chronological
        .iter()
        .enumerate()
        .map(|(i, (path, d))| {
            let (c, m) = session_time_bounds(path);
            let info = format_time_span(c.as_deref(), m.as_deref());
            let label = match d {
                Derivation::Original => "original".to_string(),
                d => format!("{} from {}", d.friendly(), i),
            };
            if info.is_empty() {
                format!("{}. {} ({label})", i + 1, path.display())
            } else {
                format!("{}. {} ({label}, {info})", i + 1, path.display())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Rollover type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloverKind {
    /// Inject lineage only.
    Quick,
    /// Lineage + sub-agent context recovery instructions.
    Context,
}

/// Default context-recovery instructions (context rollover without a custom prompt).
pub const DEFAULT_CONTEXT_INSTRUCTIONS: &str = "Recover context from the session(s) above:
1. Get a rough overview of what happened throughout the session(s)
2. Extract detailed context of the LAST task being worked on, in the MOST RECENT session-file, so work can continue smoothly.

When done, state your understanding of the task history and current state to me.";

const TERMINOLOGY: &str = "**Terminology:**
- \"trimmed\" = Long messages were truncated to free up context. Full content in parent session.
- \"rolled over\" = Work handed off to fresh session.";

/// The rollover prompt.
/// `chronological` is the chain oldest-first (current session last).
/// `subagent_instruction` defaults to `"parallel sub-agents"`.
pub fn rollover_prompt(
    chronological: &[(PathBuf, Derivation)],
    kind: RolloverKind,
    custom_prompt: Option<&str>,
    subagent_instruction: Option<&str>,
) -> String {
    let single = chronological.len() == 1;
    let first = chronological
        .first()
        .map(|(p, _)| p.display().to_string())
        .unwrap_or_default();
    let last = chronological
        .last()
        .map(|(p, _)| p.display().to_string())
        .unwrap_or_default();
    let multi_header = || {
        format!(
            "[SESSION LINEAGE]\n\nThis session continues from a chain of prior conversations. Here are the JSONL session files in CHRONOLOGICAL ORDER (oldest to newest):\n\n{}\n\n{TERMINOLOGY}\n\n",
            session_file_list(chronological)
        )
    };
    match kind {
        RolloverKind::Quick if single => format!(
            "[SESSION LINEAGE]\n\nThis session continues from a previous conversation. The prior session log is:\n\n  {first}\n\nThe file is in JSONL format. Since it can be large, use appropriate strategies (such as sub-agents if available) to carefully explore it if you need context.\n\nDo not do anything yet. Simply greet the user and await instructions on how they want to continue the work based on the above session."
        ),
        RolloverKind::Quick => format!(
            "{}Since these files can be large, use appropriate strategies (such as sub-agents if available) to carefully explore them if you need context.\n\nDo not do anything yet. Simply greet the user and await instructions on how they want to continue the work based on the above sessions.",
            multi_header()
        ),
        RolloverKind::Context => {
            let instructions = custom_prompt
                .filter(|p| !p.is_empty())
                .unwrap_or(DEFAULT_CONTEXT_INSTRUCTIONS);
            let sub = subagent_instruction.unwrap_or("parallel sub-agents");
            if single {
                format!(
                    "[SESSION LINEAGE]\n\nThis session continues from a previous conversation. The prior session log is:\n\n  {first}\n\nThe file is in JSONL format (one JSON object per line). Each line represents a message with fields like 'type' (user/assistant), 'message.content', etc.\n\n---\n\nStrategically use {sub} to recover context as instructed below. Do NOT read the file yourself - use sub-agents to preserve your context window.\n\n=== CONTEXT RECOVERY INSTRUCTIONS ===\n{instructions}\n=== END CONTEXT RECOVERY INSTRUCTIONS ==="
                )
            } else {
                format!(
                    "{}Each file is in JSONL format. The LAST file ({last}) is the most recent session.\n\n---\n\nStrategically use {sub} to recover context as instructed below. Do NOT read these files yourself - use sub-agents to preserve your context window.\n\n=== CONTEXT RECOVERY INSTRUCTIONS ===\n{instructions}\n=== END CONTEXT RECOVERY INSTRUCTIONS ===",
                    multi_header()
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::transcript::Transcript;
    use serde_json::json;
    use std::fs;

    fn write(p: &Path, first: Value) {
        fs::write(p, format!("{first}\n{{\"type\":\"user\"}}\n")).unwrap();
    }

    #[test]
    fn parent_precedence() {
        let both = json!({"continue_metadata":{"parent_session_file":"/a","parent_session_id":"x"},
                          "trim_metadata":{"parent_file":"/b"}});
        let l = parent_from_value(&both).unwrap();
        assert_eq!(
            (l.kind, l.parent_file.as_path()),
            (Derivation::Continued, Path::new("/a"))
        );
        let empty_cont = json!({"continue_metadata":{"parent_session_file":""},"trim_metadata":{"parent_file":"/b"}});
        assert_eq!(
            parent_from_value(&empty_cont).unwrap().kind,
            Derivation::Trimmed
        );
        assert!(parent_from_value(&json!({"type":"user"})).is_none());
    }

    #[test]
    fn chain_and_descendants() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.jsonl");
        let b = d.path().join("b.jsonl");
        let c = d.path().join("c.jsonl");
        fs::write(&a, "{\"type\":\"user\"}\n").unwrap();
        write(&b, json!({"trim_metadata":{"parent_file": a}}));
        write(
            &c,
            json!({"continue_metadata":{"parent_session_file": b, "parent_session_id":"b"}}),
        );
        let chain = full_chain(&c);
        assert_eq!(
            chain,
            vec![
                (c.clone(), Derivation::Continued),
                (b.clone(), Derivation::Trimmed),
                (a.clone(), Derivation::Original)
            ]
        );
        assert_eq!(full_chain(&a), vec![(a.clone(), Derivation::Original)]);
        assert_eq!(continuation_lineage(&c), vec![c.clone()]);

        // Missing parent becomes the original.
        let orphan = d.path().join("o.jsonl");
        write(
            &orphan,
            json!({"trim_metadata":{"parent_file":"/nope.jsonl"}}),
        );
        assert_eq!(
            full_chain(&orphan)[1],
            (PathBuf::from("/nope.jsonl"), Derivation::Original)
        );

        // Cycle safety.
        let x = d.path().join("x.jsonl");
        let y = d.path().join("y.jsonl");
        write(&x, json!({"trim_metadata":{"parent_file": y}}));
        write(&y, json!({"trim_metadata":{"parent_file": x}}));
        assert_eq!(full_chain(&x).len(), 2);

        let idx = child_index(&[d.path().to_path_buf()]);
        let desc = descendants(&a, &idx);
        let all: Vec<_> = desc
            .iter()
            .flat_map(|(_, k)| k.iter().map(|(p, _)| p.clone()))
            .collect();
        assert_eq!(all, vec![b.clone(), c.clone()], "rollover child c is found");
    }

    #[test]
    fn injection_text_exact() {
        let chain = vec![
            (PathBuf::from("/s/c.jsonl"), Derivation::Trimmed),
            (PathBuf::from("/s/b.jsonl"), Derivation::Continued),
            (PathBuf::from("/s/a.jsonl"), Derivation::Original),
        ];
        let note = injection_note(&chain, Derivation::Trimmed).unwrap();
        let expected = "\n\n[SESSION LINEAGE]\n\n\
This session was TRIMMED from a parent session. Long messages (tool results and assistant responses) were truncated to free up context space. Each truncated message shows a marker like:\n  [... See line N of /path/to/parent.jsonl for full content]\nYou can read the parent file at that line number to retrieve the full original content.\n\n\
Parent sessions in chronological order (oldest to newest):\n  1. /s/a.jsonl (original)\n  2. /s/b.jsonl (rolled over from 1)\n  3. /s/c.jsonl (trimmed from 2)\n\n\
Use sub-agents to read these files if you need more context.\n\n";
        assert_eq!(note, expected);
        let rolled = injection_note(&chain, Derivation::Continued).unwrap();
        assert!(rolled.contains("This session was ROLLED OVER from a parent session. Work was handed off to this fresh session with a summary of the task. The parent session is fully preserved and you can read it for detailed context if needed.\n\n"));
        assert!(injection_note(&chain[2..], Derivation::Trimmed).is_none());
    }

    #[test]
    fn inject_placement() {
        let src = br#"{"type":"summary"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x"},{"type":"text","text":"hi"}]}}
"#;
        let mut t = Transcript::from_bytes(Path::new("/x"), Agent::Claude, src);
        assert_eq!(
            inject_into_first_user_message(Agent::Claude, &mut t.lines, "N:"),
            Some(1)
        );
        assert_eq!(
            t.lines[1]
                .value()
                .unwrap()
                .pointer("/message/content/1/text"),
            Some(&json!("N:hi"))
        );

        let src = br#"{"type":"user","message":{"role":"user","content":[{"type":"image"}]}}"#;
        let mut t = Transcript::from_bytes(Path::new("/x"), Agent::Claude, src);
        inject_into_first_user_message(Agent::Claude, &mut t.lines, "N:");
        assert_eq!(
            t.lines[0].value().unwrap().pointer("/message/content/0"),
            Some(&json!({"type":"text","text":"N:"}))
        );

        let src = br#"{"type":"session_meta","payload":{}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}
"#;
        let mut t = Transcript::from_bytes(Path::new("/x"), Agent::Codex, src);
        assert_eq!(
            inject_into_first_user_message(Agent::Codex, &mut t.lines, "N:"),
            Some(1)
        );
        assert_eq!(
            t.lines[1]
                .value()
                .unwrap()
                .pointer("/payload/content/0/text"),
            Some(&json!("N:hello"))
        );
    }

    #[test]
    fn rollover_prompts() {
        let one = vec![(PathBuf::from("/s/a.jsonl"), Derivation::Original)];
        let q = rollover_prompt(&one, RolloverKind::Quick, None, None);
        assert!(q.starts_with("[SESSION LINEAGE]\n\nThis session continues from a previous conversation. The prior session log is:\n\n  /s/a.jsonl\n\n"));
        let c = rollover_prompt(
            &one,
            RolloverKind::Context,
            None,
            Some("PARALLEL HAIKU SUB-AGENTS"),
        );
        assert!(c.contains("Strategically use PARALLEL HAIKU SUB-AGENTS to recover"));
        assert!(c.ends_with("When done, state your understanding of the task history and current state to me.\n=== END CONTEXT RECOVERY INSTRUCTIONS ==="));
        let two = vec![
            (PathBuf::from("/nope/a.jsonl"), Derivation::Original),
            (PathBuf::from("/nope/b.jsonl"), Derivation::Trimmed),
        ];
        let m = rollover_prompt(&two, RolloverKind::Context, Some("do X"), None);
        assert!(m.contains(
            "1. /nope/a.jsonl (original)\n2. /nope/b.jsonl (trimmed from 1)\n\n**Terminology:**"
        ));
        assert!(m.contains("The LAST file (/nope/b.jsonl) is the most recent session."));
        assert!(m.contains("=== CONTEXT RECOVERY INSTRUCTIONS ===\ndo X\n"));
    }

    #[test]
    fn spans() {
        assert!(
            format_time_span(Some("2026-10-01T00:00:00Z"), Some("2026-10-03T00:00:00Z"))
                .starts_with("2d, ")
        );
        assert!(
            format_time_span(Some("2026-10-01T00:00:00Z"), Some("2026-10-01T03:10:00Z"))
                .starts_with("3h, ")
        );
        assert!(
            format_time_span(Some("2026-10-01T00:00:00Z"), Some("2026-10-01T00:05:00Z"))
                .starts_with("5m, ")
        );
        assert!(
            format_time_span(Some("2026-10-01T00:00:00Z"), Some("2026-10-01T00:00:30Z"))
                .starts_with("<1m, ")
        );
        assert_eq!(format_time_span(None, None), "");
    }
}
