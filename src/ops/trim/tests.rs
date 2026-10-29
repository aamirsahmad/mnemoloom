use super::*;
use crate::session::pyjson;
use crate::session::testutil::*;
use serde_json::json;

/// `<dir>/claude`, `<dir>/codex`.
fn homes(dir: &Path) -> Homes {
    Homes::new(dir.join("claude"), dir.join("codex"))
}

fn line(v: Value) -> Line {
    Line::from_raw(serde_json::to_vec(&v).unwrap())
}

fn transcript(agent: Agent, values: &[Value]) -> Transcript {
    let body = values
        .iter()
        .map(|v| serde_json::to_string(v).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    Transcript::from_bytes(Path::new("/p/parent.jsonl"), agent, body.as_bytes())
}

fn assistant_tool_use(id: &str, name: &str, input: Value) -> Value {
    json!({"type": "assistant", "sessionId": CLAUDE_ID, "message": {"role": "assistant",
        "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}})
}

fn tool_result(id: &str, content: Value) -> Value {
    json!({"type": "user", "sessionId": CLAUDE_ID, "message": {"role": "user",
        "content": [{"type": "tool_result", "tool_use_id": id, "content": content}]}})
}

fn assistant_text(text: &str) -> Value {
    json!({"type": "assistant", "sessionId": CLAUDE_ID,
        "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}})
}

fn opts(threshold: usize) -> TrimOptions {
    TrimOptions {
        threshold,
        ..TrimOptions::default()
    }
}

fn content(t: &Transcript, i: usize) -> Value {
    t.lines[i].value().unwrap()["message"]["content"][0].clone()
}

#[test]
fn claude_tool_results_threshold_and_notice() {
    let mut t = transcript(
        Agent::Claude,
        &[
            assistant_tool_use("t1", "Bash", json!({"command": "ls"})),
            tool_result("t1", json!("x".repeat(1200))),
            tool_result("t1", json!("y".repeat(500))),
            tool_result("missing", json!("z".repeat(501))),
        ],
    );
    let stats = trim_lines(
        Agent::Claude,
        &mut t.lines,
        &opts(500),
        "/p/parent.jsonl",
        None,
    );
    let want = format!(
        "{}\n\n[...truncated - original content was 1,200 characters, showing first 500. See line 2 of /p/parent.jsonl for full content]",
        "x".repeat(500)
    );
    assert_eq!(content(&t, 1)["content"], json!(want));
    assert!(!t.lines[2].is_modified(), "exactly threshold is kept");
    assert!(
        !t.lines[3].is_modified(),
        "501 chars: truncation would not be shorter"
    );
    assert_eq!(stats.num_tools_trimmed, 1);
    assert_eq!(stats.chars_saved, 1200 - char_len(&want) as i64);
    assert_eq!(stats.num_assistant_trimmed, 0);
}

#[test]
fn claude_tool_filter_and_tool_use_inputs() {
    let big = "w".repeat(2000);
    let mut t = transcript(
        Agent::Claude,
        &[
            assistant_tool_use("t1", "Bash", json!({"command": big, "n": 5})),
            tool_result("t1", json!(big)),
            assistant_tool_use("t2", "Write", json!({"content": "q".repeat(10)})),
        ],
    );
    let o = TrimOptions {
        target_tools: Some(parse_tools("Read, edit")),
        ..opts(500)
    };
    let stats = trim_lines(Agent::Claude, &mut t.lines, &o, "/p/parent.jsonl", None);
    // tool_use inputs ignore --tools; results honour it.
    assert_eq!(
        content(&t, 0)["input"]["command"],
        json!(format!(
            "{}\n\n[...truncated command - was 2,000 chars. See line 1 of /p/parent.jsonl]",
            "w".repeat(500)
        ))
    );
    assert_eq!(content(&t, 0)["input"]["n"], json!(5));
    assert!(!t.lines[1].is_modified());
    assert!(!t.lines[2].is_modified());
    assert_eq!(stats.num_tools_trimmed, 1);
}

#[test]
fn claude_tool_use_input_at_threshold_grows_but_is_not_counted() {
    let mut t = transcript(
        Agent::Claude,
        &[assistant_tool_use(
            "t1",
            "Write",
            json!({"content": "a".repeat(20)}),
        )],
    );
    let stats = trim_lines(Agent::Claude, &mut t.lines, &opts(20), "/x", None);
    assert!(t.lines[0].is_modified(), "rewritten even though longer");
    assert_eq!(stats, TrimStats::default());
}

#[test]
fn claude_list_results_flatten_python_style() {
    let items = json!([
        {"type": "text", "text": "a".repeat(300)},
        {"type": "image", "source": {"type": "base64", "data": "b".repeat(400)}}
    ]);
    let mut t = transcript(Agent::Claude, &[tool_result("t", items)]);
    trim_lines(Agent::Claude, &mut t.lines, &opts(500), "/x", None);
    let out = content(&t, 0)["content"].as_str().unwrap().to_string();
    let want_prefix = format!(
        "{}{{'type': 'image', 'source': {{'type': 'base64', 'data': '{}",
        "a".repeat(300),
        "b".repeat(144)
    );
    assert!(out.starts_with(&want_prefix), "{out}");
    let full = 300 + "{'type': 'image', 'source': {'type': 'base64', 'data': ''}}".len() + 400;
    assert!(out.contains(&format!(
        "original content was {} characters",
        group(full as i64)
    )));
}

#[test]
fn assistant_selection_first_and_all_but_last() {
    let texts = [600, 100, 700, 800].map(|n| assistant_text(&"m".repeat(n)));
    let trimmed_lines = |n: i64| {
        let mut t = transcript(Agent::Claude, &texts);
        let o = TrimOptions {
            trim_assistant: Some(n),
            ..opts(500)
        };
        let stats = trim_lines(Agent::Claude, &mut t.lines, &o, "/p/parent.jsonl", None);
        let lines: Vec<usize> = (0..4).filter(|&i| t.lines[i].is_modified()).collect();
        (lines, stats.num_assistant_trimmed)
    };
    assert_eq!(trimmed_lines(1), (vec![0], 1));
    assert_eq!(trimmed_lines(-1), (vec![0, 2], 2));
    assert_eq!(trimmed_lines(-5), (vec![], 0));
    assert_eq!(trimmed_lines(10), (vec![0, 2, 3], 3));
    assert_eq!(trimmed_lines(0), (vec![], 0));

    let mut t = transcript(Agent::Claude, &texts);
    let o = TrimOptions {
        trim_assistant: Some(1),
        ..opts(500)
    };
    trim_lines(Agent::Claude, &mut t.lines, &o, "/p/parent.jsonl", None);
    assert_eq!(
        content(&t, 0)["text"],
        json!(
            "[Assistant message trimmed - original content was 600 characters. See line 1 of /p/parent.jsonl for full content]"
        )
    );
}

#[test]
fn already_trimmed_content_is_skipped() {
    let placeholder = format!("[Assistant message trimmed - {}", "p".repeat(900));
    let mut t = transcript(
        Agent::Claude,
        &[
            assistant_text(&placeholder),
            assistant_text(&"n".repeat(900)),
            tool_result("t", json!(format!("[...truncated {}", "r".repeat(900)))),
        ],
    );
    let o = TrimOptions {
        trim_assistant: Some(1),
        ..opts(500)
    };
    let stats = trim_lines(Agent::Claude, &mut t.lines, &o, "/x", None);
    assert!(!t.lines[0].is_modified() && !t.lines[2].is_modified());
    assert!(
        t.lines[1].is_modified(),
        "placeholder does not occupy the +1 slot"
    );
    assert_eq!(stats.num_assistant_trimmed, 1);
}

#[test]
fn context_full_markers_and_usage() {
    let mut t = transcript(
        Agent::Claude,
        &[
            json!({"type": "assistant", "error": "invalid_request", "isApiErrorMessage": true,
                "message": {"model": "<synthetic>", "content": [{"type": "text", "text": "Prompt is too long"}]}}),
            json!({"type": "assistant", "message": {"model": "m", "content": [],
                "usage": {"input_tokens": 5, "cache": {"x": 1.5, "y": null}, "tier": "standard"}}}),
            json!({"type": "assistant", "message": {"model": "m", "content": [],
                "usage": {"input_tokens": 0, "f": 0.0, "b": false}}}),
        ],
    );
    trim_lines(Agent::Claude, &mut t.lines, &opts(500), "/x", None);
    assert_eq!(
        t.lines[0].value().unwrap(),
        &json!({"type": "assistant", "error": null, "isApiErrorMessage": false,
            "message": {"model": "trimmed", "content": [{"type": "text", "text": "[Context limit reached in parent session - trimmed]"}]}})
    );
    assert_eq!(
        t.lines[1].value().unwrap()["message"]["usage"],
        json!({"input_tokens": 0, "cache": {"x": 0, "y": null}, "tier": "standard"})
    );
    assert!(
        !t.lines[2].is_modified(),
        "already-zero usage (Python ==) is untouched"
    );
}

#[test]
fn untouched_lines_are_byte_identical() {
    let big = "z".repeat(1000);
    let mut src = b"{\"type\":  \"summary\" , \"x\": 1}\n\nnot json\n[1, 2]\n{\"type\":\"user\",\"bad\":\"\xff\"}\n".to_vec();
    src.extend_from_slice(
        serde_json::to_string(&tool_result("t", json!(big)))
            .unwrap()
            .as_bytes(),
    );
    src.extend_from_slice(
        b"\n{\"type\":\"user\",\"message\":{\"content\":\"hi\"},\"sessionId\":\"old\"}\n",
    );
    let mut t = Transcript::from_bytes(Path::new("/p.jsonl"), Agent::Claude, &src);
    trim_lines(Agent::Claude, &mut t.lines, &opts(500), "/p.jsonl", None);
    let out = t.to_bytes(WriteStyle::Python);
    let (a, b): (Vec<&[u8]>, Vec<&[u8]>) = (
        src.split(|&c| c == b'\n').collect(),
        out.split(|&c| c == b'\n').collect(),
    );
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        if i == 5 {
            assert_ne!(x, y);
            assert!(
                y.starts_with(b"{\"type\": \"user\", \"sessionId\": "),
                "Python json.dumps style"
            );
        } else {
            assert_eq!(x, y, "line {i}");
        }
    }
}

#[test]
fn session_id_rewrite() {
    let mut t = transcript(
        Agent::Claude,
        &[
            json!({"type": "x", "sessionId": "old"}),
            json!({"type": "y"}),
        ],
    );
    trim_lines(Agent::Claude, &mut t.lines, &opts(500), "/x", Some("new"));
    assert_eq!(t.lines[0].value().unwrap()["sessionId"], "new");
    assert!(!t.lines[1].is_modified());
}

fn codex_output(call: &str, output: &str) -> Value {
    json!({"timestamp": "t", "type": "response_item",
        "payload": {"type": "function_call_output", "call_id": call, "output": output}})
}

#[test]
fn codex_outputs_meta_and_assistant() {
    let wrapped = pyjson::dumps(&json!({"output": "o".repeat(1000), "metadata": {"exit_code": 0}}));
    let mut t = transcript(
        Agent::Codex,
        &[
            json!({"timestamp": "t", "type": "session_meta", "payload": {"id": "old", "cwd": "/w"}}),
            json!({"timestamp": "t", "type": "response_item", "payload": {"type": "function_call", "name": "shell", "call_id": "c1", "arguments": "{}"}}),
            codex_output("c1", &wrapped),
            codex_output("c1", &"p".repeat(800)),
            codex_output("c1", &"s".repeat(500)),
            json!({"timestamp": "t", "type": "response_item", "payload": {"type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "a".repeat(900)}]}}),
            json!({"timestamp": "t", "type": "event_msg", "payload": {"type": "agent_message", "message": "a".repeat(900)}}),
        ],
    );
    let o = TrimOptions {
        trim_assistant: Some(0),
        ..opts(500)
    };
    let stats = trim_lines(Agent::Codex, &mut t.lines, &o, "/p/r.jsonl", Some("new-id"));
    assert_eq!(t.lines[0].value().unwrap()["payload"]["id"], "new-id");
    assert!(!t.lines[1].is_modified());
    let notice = |n: usize, line: usize| {
        format!(
            "\n\n[...truncated - original content was {} characters, showing first 500. See line {line} of /p/r.jsonl for full content]",
            group(n as i64)
        )
    };
    assert_eq!(
        t.lines[2].value().unwrap()["payload"]["output"],
        json!(pyjson::dumps(
            &json!({"output": format!("{}{}", "o".repeat(500), notice(1000, 3)), "metadata": {"exit_code": 0}})
        ))
    );
    assert_eq!(
        t.lines[3].value().unwrap()["payload"]["output"],
        json!(pyjson::dumps(
            &json!({"output": format!("{}{}", "p".repeat(500), notice(800, 4)), "metadata": {}})
        ))
    );
    assert!(!t.lines[4].is_modified());
    assert!(!t.lines[5].is_modified() && !t.lines[6].is_modified());
    assert_eq!(stats.num_tools_trimmed, 2);

    let o = TrimOptions {
        trim_assistant: Some(1),
        target_tools: Some(parse_tools("apply_patch")),
        ..opts(500)
    };
    let mut t2 = transcript(
        Agent::Codex,
        &[
            codex_output("c1", &"p".repeat(800)),
            json!({"type": "response_item", "payload": {"type": "message", "role": "assistant",
            "content": [{"type": "output_text", "text": "a".repeat(900)}]}}),
        ],
    );
    let stats = trim_lines(Agent::Codex, &mut t2.lines, &o, "/p/r.jsonl", None);
    assert!(!t2.lines[0].is_modified(), "Unknown tool filtered out");
    assert_eq!(
        t2.lines[1].value().unwrap()["payload"]["content"][0]["text"],
        json!(
            "[Assistant message trimmed - original content was 900 characters. See line 2 of /p/r.jsonl for full content]"
        )
    );
    assert_eq!(stats.num_assistant_trimmed, 1);
}

fn big_claude_session(id: &str, cwd: &str) -> Vec<String> {
    let mut lines = claude_lines(id, cwd);
    lines.insert(
        6,
        serde_json::to_string(&tool_result("toolu_1", json!("L".repeat(5000)))).unwrap(),
    );
    lines
}

#[test]
fn trim_and_create_claude_session() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("claude");
    let src = write_claude(
        &home,
        CLAUDE_ID,
        "/w/p",
        &big_claude_session(CLAUDE_ID, "/w/p"),
    );
    let before = std::fs::read(&src).unwrap();
    let out = trim_and_create_session(
        None,
        &src,
        &TrimOptions::default(),
        None,
        &homes(dir.path()),
    )
    .unwrap();
    assert_eq!(std::fs::read(&src).unwrap(), before, "source untouched");
    let id = out.session_id.clone().unwrap();
    let file = out.output_file.clone().unwrap();
    assert_eq!(file, src.with_file_name(format!("{id}.jsonl")));
    assert_eq!(out.agent, Agent::Claude);
    assert_eq!(out.stats.num_tools_trimmed, 1);

    let t = Transcript::load(&file, Agent::Claude).unwrap();
    let first = t.lines[0].value().unwrap();
    let meta = &first["trim_metadata"];
    assert_eq!(meta["parent_file"], src.display().to_string());
    assert_eq!(
        meta["trim_params"],
        json!({"threshold": 500, "tools": null, "trim_assistant_messages": null})
    );
    assert_eq!(
        meta["stats"],
        json!({"num_tools_trimmed": 1, "num_assistant_trimmed": 0, "tokens_saved": out.stats.tokens_saved()})
    );
    let ts = meta["trimmed_at"].as_str().unwrap();
    assert!(ts.ends_with("+00:00") && ts.contains('T'), "{ts}");
    assert_eq!(
        first.as_object().unwrap().keys().next_back().unwrap(),
        "trim_metadata"
    );
    for v in t.lines.iter().filter_map(Line::value) {
        if let Some(sid) = v.get("sessionId") {
            assert_eq!(sid, &json!(id));
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        !text.contains("[SESSION LINEAGE]"),
        "first-generation trim: no note"
    );
}

#[test]
fn trim_metadata_replaces_continue_metadata_and_injects_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("claude");
    let root = write_claude(&home, CLAUDE_ID2, "/w/p", &claude_lines(CLAUDE_ID2, "/w/p"));
    let mut lines = big_claude_session(CLAUDE_ID, "/w/p");
    let mut first: Value = serde_json::from_str(&lines[0]).unwrap();
    first["continue_metadata"] = json!({"parent_session_id": CLAUDE_ID2,
        "parent_session_file": root.display().to_string(), "continued_at": "x"});
    lines[0] = serde_json::to_string(&first).unwrap();
    let src = write_claude(&home, CLAUDE_ID, "/w/p", &lines);
    let out = trim_and_create_session(
        Some(Agent::Claude),
        &src,
        &TrimOptions::default(),
        None,
        &homes(dir.path()),
    )
    .unwrap();
    let t = Transcript::load(out.output_file.as_ref().unwrap(), Agent::Claude).unwrap();
    let first = t.lines[0].value().unwrap();
    assert!(first.get("continue_metadata").is_none());
    assert!(first.get("trim_metadata").is_some());
    // First `type == "user"` line is the meta line (index 2 of the fixture).
    let note = t.lines[2].value().unwrap()["message"]["content"]
        .as_str()
        .unwrap()
        .to_string();
    let want = format!(
        "\n\n[SESSION LINEAGE]\n\nThis session was TRIMMED from a parent session. Long messages (tool results and assistant responses) were truncated to free up context space. Each truncated message shows a marker like:\n  [... See line N of /path/to/parent.jsonl for full content]\nYou can read the parent file at that line number to retrieve the full original content.\n\nParent sessions in chronological order (oldest to newest):\n  1. {} (original)\n  2. {} (rolled over from 1)\n\nUse sub-agents to read these files if you need more context.\n\n<command-name>/clear</command-name>",
        root.display(),
        src.display()
    );
    assert_eq!(note, want);
}

#[test]
fn small_savings_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_claude(dir.path(), CLAUDE_ID, "/w", &claude_lines(CLAUDE_ID, "/w"));
    let out = trim_and_create_session(
        None,
        &src,
        &TrimOptions::default(),
        None,
        &homes(dir.path()),
    )
    .unwrap();
    assert!(out.nothing_to_trim() && out.output_file.is_none());
    assert_eq!(std::fs::read_dir(src.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn codex_output_path_and_tools_list() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("codex");
    let mut lines = codex_lines(CODEX_ID, "/w", "cli");
    lines[7] = serde_json::to_string(&codex_output("call_1", &"x".repeat(5000))).unwrap();
    let src = write_codex(&home, CODEX_ID, "2026-05-07T12-57-30", &lines);
    let o = TrimOptions {
        target_tools: Some(parse_tools("exec_command,Bash")),
        ..TrimOptions::default()
    };
    let out = trim_and_create_session(None, &src, &o, None, &homes(dir.path())).unwrap();
    assert_eq!(out.agent, Agent::Codex);
    let file = out.output_file.unwrap();
    let id = out.session_id.unwrap();
    let now = chrono::Local::now();
    assert_eq!(
        file.parent().unwrap(),
        home.join("sessions")
            .join(now.format("%Y/%m/%d").to_string())
    );
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("rollout-") && name.ends_with(&format!("-{id}.jsonl")));
    assert_eq!(session::codex_filename_id(&name), Some(id.clone()));
    let t = Transcript::load(&file, Agent::Codex).unwrap();
    let first = t.lines[0].value().unwrap();
    assert_eq!(first["payload"]["id"], json!(id));
    assert_eq!(
        first["trim_metadata"]["trim_params"]["tools"],
        json!(["bash", "exec_command"])
    );
}

#[test]
fn agent_detection() {
    let dir = tempfile::tempdir().unwrap();
    let c = write_raw(&dir.path().join("c.jsonl"), &claude_lines(CLAUDE_ID, "/w"));
    let x = write_raw(
        &dir.path().join("x.jsonl"),
        &codex_lines(CODEX_ID, "/w", "cli"),
    );
    let none = write_raw(
        &dir.path().join("n.jsonl"),
        &["{\"a\":1}".to_string(), "text".to_string()],
    );
    assert_eq!(detect_agent(&c).unwrap(), Agent::Claude);
    assert_eq!(detect_agent(&x).unwrap(), Agent::Codex);
    assert_eq!(detect_agent(&none).unwrap(), Agent::Claude);
    assert_eq!(detect_agent_strict(&none).unwrap(), None);
    assert_eq!(detect_agent_strict(&x).unwrap(), Some(Agent::Codex));
}

#[test]
fn helpers() {
    assert_eq!(char_len("日本😀"), 3);
    assert_eq!(take_chars("日本😀", 2), "日本");
    assert_eq!(take_chars("héllo", 2), "hé");
    assert_eq!(take_chars("hi", 5), "hi");
    assert_eq!(group(-1234567), "-1,234,567");
    let mut lines = vec![line(json!({"a": 1, "trim_metadata": {}, "b": 2}))];
    assert!(set_first_line_metadata(
        &mut lines,
        Some("trim_metadata"),
        "continue_metadata",
        json!(1)
    ));
    assert_eq!(
        serde_json::to_string(lines[0].value().unwrap()).unwrap(),
        r#"{"a":1,"b":2,"continue_metadata":1}"#
    );
    let mut lines = vec![line(json!({"trim_metadata": 0, "b": 2}))];
    set_first_line_metadata(&mut lines, None, "trim_metadata", json!(1));
    assert_eq!(
        serde_json::to_string(lines[0].value().unwrap()).unwrap(),
        r#"{"trim_metadata":1,"b":2}"#,
        "existing key keeps its position (dict.update)"
    );
    assert_eq!(
        parse_tools(" Bash,,read ").into_iter().collect::<Vec<_>>(),
        ["", "bash", "read"]
    );
}

mod in_place {
    use super::*;

    fn session(dir: &Path) -> PathBuf {
        write_claude(
            &dir.join("claude"),
            CLAUDE_ID,
            "/w/p",
            &big_claude_session(CLAUDE_ID, "/w/p"),
        )
    }

    fn bak_files(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with(".jsonl.bak"))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn dry_run_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let src = session(dir.path());
        let before = std::fs::read(&src).unwrap();
        let r = trim_session_in_place(&src, &TrimOptions::default(), true).unwrap();
        assert!(r.dry_run && !r.applied && !r.nothing_to_trim);
        assert_eq!(r.backup_file, None);
        assert_eq!(r.size_before, before.len() as u64);
        assert!(r.size_after < r.size_before);
        assert_eq!(std::fs::read(&src).unwrap(), before);
        assert_eq!(std::fs::read_dir(src.parent().unwrap()).unwrap().count(), 1);
        let j = r.to_json();
        let keys: Vec<&String> = j.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "applied",
                "dry_run",
                "nothing_to_trim",
                "num_tools_trimmed",
                "num_assistant_trimmed",
                "chars_saved",
                "tokens_saved",
                "backup_file",
                "session_file",
                "size_before",
                "size_after"
            ]
        );
    }

    #[test]
    fn apply_backs_up_and_keeps_identity() {
        let dir = tempfile::tempdir().unwrap();
        let src = session(dir.path());
        let before = std::fs::read(&src).unwrap();
        let r = trim_session_in_place(&src, &TrimOptions::default(), false).unwrap();
        assert!(r.applied && !r.dry_run);
        let bak = r.backup_file.clone().unwrap();
        let listed = bak_files(src.parent().unwrap());
        assert_eq!(listed.len(), 1);
        assert_eq!(std::fs::canonicalize(&listed[0]).unwrap(), bak);
        let name = bak.file_name().unwrap().to_string_lossy().into_owned();
        let re = regex::Regex::new(&format!(
            r"^{CLAUDE_ID}\.pre-trim-\d{{8}}-\d{{6}}\.jsonl\.bak$"
        ))
        .unwrap();
        assert!(re.is_match(&name), "{name}");
        assert_eq!(
            std::fs::read(&bak).unwrap(),
            before,
            "backup is the original"
        );
        let after = std::fs::read_to_string(&src).unwrap();
        assert_eq!(r.size_after, after.len() as u64);
        assert_eq!(
            after.lines().count(),
            before.split(|&b| b == b'\n').count() - 1
        );
        assert!(after.contains(&format!("See line 7 of {} for full content", bak.display())));
        assert!(!after.contains("trim_metadata") && !after.contains("[SESSION LINEAGE]"));
        assert!(
            after
                .lines()
                .filter(|l| l.contains("sessionId"))
                .all(|l| l.contains(CLAUDE_ID))
        );
        // Placeholders are never re-trimmed: a second run has nothing to do.
        let again = trim_session_in_place(&src, &opts(100), false).unwrap();
        assert!(again.nothing_to_trim && !again.applied);
        assert_eq!(
            bak_files(src.parent().unwrap()).len(),
            1,
            "reserved backup released"
        );
    }

    #[test]
    fn backup_names_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let src = session(dir.path());
        let first = reserve_backup_for_test(&src);
        let second = reserve_backup_for_test(&src);
        assert_ne!(first, second);
        assert!(second.to_string_lossy().ends_with("-2.jsonl.bak"));
    }

    fn reserve_backup_for_test(p: &Path) -> PathBuf {
        super::inplace::reserve_backup(p).unwrap()
    }

    /// A writer holding the pre-swap fd (forces the tail splice) and a
    /// by-path appender (races the splice) run during an applied trim:
    /// every line must survive exactly once.
    #[test]
    fn concurrent_appenders_lose_nothing() {
        use std::io::Write;
        use std::os::unix::fs::MetadataExt;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(session(dir.path())).unwrap();
        let ino0 = std::fs::metadata(&path).unwrap().ino();
        let wait_for_swap = {
            let path = path.clone();
            move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                while std::fs::metadata(&path).unwrap().ino() == ino0 {
                    assert!(Instant::now() < deadline, "trim never swapped");
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
        };
        let probe = |w: &str, i: usize| format!("{{\"type\":\"probe\",\"w\":\"{w}\",\"i\":{i}}}\n");
        let mut old_fd = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let old_writer = std::thread::spawn({
            let wait = wait_for_swap.clone();
            move || {
                wait();
                for i in 0..150 {
                    old_fd.write_all(probe("old", i).as_bytes()).unwrap();
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        });
        let path_writer = std::thread::spawn({
            let path = path.clone();
            move || {
                wait_for_swap();
                for i in 0..300 {
                    let mut f = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&path)
                        .unwrap();
                    f.write_all(probe("path", i).as_bytes()).unwrap();
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        });
        let r = trim_session_in_place(&path, &TrimOptions::default(), false).unwrap();
        old_writer.join().unwrap();
        path_writer.join().unwrap();
        assert!(r.applied);
        let text = std::fs::read_to_string(&path).unwrap();
        for (w, n) in [("old", 150), ("path", 300)] {
            for i in 0..n {
                let line = probe(w, i);
                assert_eq!(text.matches(line.as_str()).count(), 1, "{w} line {i}");
            }
        }
        let positions: Vec<usize> = (0..150)
            .map(|i| text.find(probe("old", i).as_str()).unwrap())
            .collect();
        assert!(
            positions.windows(2).all(|p| p[0] < p[1]),
            "old-fd lines keep their order"
        );
        assert!(
            text.lines()
                .all(|l| serde_json::from_str::<Value>(l).is_ok()),
            "no torn lines"
        );
        let backup = std::fs::read_to_string(r.backup_file.unwrap()).unwrap();
        assert!((0..150).all(|i| backup.contains(probe("old", i).as_str())));
    }

    #[test]
    fn rejects_non_claude_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let x = write_raw(
            &dir.path().join("r.jsonl"),
            &codex_lines(CODEX_ID, "/w", "cli"),
        );
        let err = trim_session_in_place(&x, &TrimOptions::default(), false).unwrap_err();
        assert_eq!(
            err.to_string(),
            "In-place trim only supports Claude sessions (detected: codex)"
        );
        let e = write_raw(&dir.path().join("e.jsonl"), &["hello".to_string()]);
        let err = trim_session_in_place(&e, &TrimOptions::default(), false).unwrap_err();
        assert_eq!(
            err.to_string(),
            "In-place trim only supports Claude sessions (detected: unknown)"
        );
        let missing = dir.path().join("nope.jsonl");
        let err = trim_session_in_place(&missing, &TrimOptions::default(), false).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("Session file not found: {}", missing.display())
        );
    }

    #[test]
    fn json_errors_exit_1() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("claude")).unwrap();
        std::fs::create_dir_all(dir.path().join("codex")).unwrap();
        let global = GlobalArgs {
            claude_home: Some(dir.path().join("claude")),
            codex_home: Some(dir.path().join("codex")),
            pi_home: Some(dir.path().join("pi")),
            opencode_home: Some(dir.path().join("opencode")),
        };
        let args = |session: &str| TrimInPlaceArgs {
            session: session.to_string(),
            tools: None,
            threshold: 500,
            trim_assistant: None,
            dry_run: true,
            json: true,
        };
        assert_eq!(
            trim_in_place(&global, &args("no-such-session")).unwrap(),
            ExitCode::from(1)
        );
        let src = session(dir.path());
        assert_eq!(
            trim_in_place(&global, &args(&src.display().to_string())).unwrap(),
            ExitCode::SUCCESS
        );
        assert_eq!(
            trim_in_place(&global, &args(CLAUDE_ID)).unwrap(),
            ExitCode::SUCCESS
        );
        let mut human = args("no-such-session");
        human.json = false;
        let err = trim_in_place(&global, &human).unwrap_err();
        assert_eq!(
            err.downcast_ref::<Exit>().unwrap().message.as_deref(),
            Some("Error: Session not found: no-such-session")
        );
    }
}

#[test]
fn archived_codex_trim_lands_in_codex_home() {
    let dir = tempfile::tempdir().unwrap();
    let h = homes(dir.path());
    let mut lines = codex_lines(CODEX_ID, "/w", "cli");
    lines[7] = serde_json::to_string(&codex_output("call_1", &"x".repeat(5000))).unwrap();
    let src = write_raw(
        &h.codex
            .join("archived_sessions")
            .join(format!("rollout-2026-05-07T12-57-30-{CODEX_ID}.jsonl")),
        &lines,
    );
    let out = trim_and_create_session(None, &src, &TrimOptions::default(), None, &h).unwrap();
    let file = out.output_file.unwrap();
    assert!(
        file.starts_with(h.codex.join("sessions")),
        "{}",
        file.display()
    );

    // Nothing to trim: no dated directories are created.
    let small = write_raw(
        &dir.path()
            .join("x")
            .join(format!("rollout-2026-05-07T12-57-30-{CODEX_ID2}.jsonl")),
        &codex_lines(CODEX_ID2, "/w", "cli"),
    );
    let h2 = homes(&dir.path().join("other"));
    let out = trim_and_create_session(None, &small, &TrimOptions::default(), None, &h2).unwrap();
    assert!(out.nothing_to_trim());
    assert!(!h2.codex.exists());
}

#[test]
fn in_place_empty_tools_means_all() {
    assert_eq!(inplace::tool_filter(Some("")), None);
    assert_eq!(inplace::tool_filter(None), None);
    assert_eq!(
        inplace::tool_filter(Some("Bash,,")),
        Some(BTreeSet::from(["bash".to_string()]))
    );
}
