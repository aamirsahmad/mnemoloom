use super::*;
use crate::session::testutil::*;
use std::os::unix::fs::PermissionsExt;

fn fake_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(format!("fake-{name}"));
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn request<'a>(
    source: &'a Path,
    target: Agent,
    kind: RolloverKind,
    custom: Option<&'a str>,
) -> Request<'a> {
    let mut chain = full_chain(source);
    chain.reverse();
    Request {
        source,
        chain,
        target,
        kind,
        custom_prompt: custom,
        config: Config::default(),
    }
}

#[test]
fn prompt_quick_single_is_verbatim() {
    let src = Path::new("/h/projects/p/a.jsonl");
    let req = request(src, Agent::Claude, RolloverKind::Quick, None);
    assert_eq!(
        build_prompt(&req),
        "[SESSION LINEAGE]\n\nThis session continues from a previous conversation. The prior session log is:\n\n  /h/projects/p/a.jsonl\n\nThe file is in JSONL format. Since it can be large, use appropriate strategies (such as sub-agents if available) to carefully explore it if you need context.\n\nDo not do anything yet. Simply greet the user and await instructions on how they want to continue the work based on the above session."
    );
}

#[test]
fn prompt_context_subagent_phrase_per_target() {
    let src = Path::new("/h/a.jsonl");
    let claude = build_prompt(&request(src, Agent::Claude, RolloverKind::Context, None));
    assert!(claude.contains("Strategically use PARALLEL HAIKU SUB-AGENTS to recover context"));
    assert!(claude.contains("=== CONTEXT RECOVERY INSTRUCTIONS ===\nRecover context from the session(s) above:\n1. Get a rough overview"));
    let mut req = request(
        src,
        Agent::Claude,
        RolloverKind::Context,
        Some("Focus on auth"),
    );
    req.config.claude_subagent_model = "sonnet".into();
    let custom = build_prompt(&req);
    assert!(custom.contains("PARALLEL SONNET SUB-AGENTS"));
    assert!(custom.ends_with("=== CONTEXT RECOVERY INSTRUCTIONS ===\nFocus on auth\n=== END CONTEXT RECOVERY INSTRUCTIONS ==="));
    let codex = build_prompt(&request(src, Agent::Codex, RolloverKind::Context, None));
    assert!(
        codex.contains("Strategically use parallel sub-agents (if available) to recover context")
    );
}

#[test]
fn prompt_multi_file_lists_chain() {
    let dir = tempfile::tempdir().unwrap();
    let root = write_raw(
        &dir.path().join("root.jsonl"),
        &claude_lines(CLAUDE_ID2, "/w"),
    );
    let mut lines = claude_lines(CLAUDE_ID, "/w");
    lines[0] = format!(
        r#"{{"type":"x","trim_metadata":{{"parent_file":"{}"}}}}"#,
        root.display()
    );
    let child = write_raw(&dir.path().join("child.jsonl"), &lines);
    let p = build_prompt(&request(&child, Agent::Claude, RolloverKind::Quick, None));
    assert!(p.starts_with(
        "[SESSION LINEAGE]\n\nThis session continues from a chain of prior conversations."
    ));
    assert!(p.contains(&format!("1. {} (original, ", root.display())));
    assert!(p.contains(&format!("2. {} (trimmed from 1, ", child.display())));
    assert!(p.contains("**Terminology:**"));
}

#[test]
fn continue_metadata_replaces_trim_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = claude_lines(CLAUDE_ID, "/w");
    lines[0] =
        r#"{"type":"permission-mode","trim_metadata":{"parent_file":"/old"},"sessionId":"s"}"#
            .into();
    let f = write_raw(&dir.path().join("n.jsonl"), &lines);
    add_continue_metadata(
        &f,
        Agent::Claude,
        "parent-stem",
        Path::new("/p/parent-stem.jsonl"),
    )
    .unwrap();
    let text = std::fs::read_to_string(&f).unwrap();
    let out: Vec<&str> = text.lines().collect();
    let first: Value = serde_json::from_str(out[0]).unwrap();
    assert!(first.get("trim_metadata").is_none());
    let cm = &first["continue_metadata"];
    assert_eq!(cm["parent_session_id"], "parent-stem");
    assert_eq!(cm["parent_session_file"], "/p/parent-stem.jsonl");
    assert!(cm["continued_at"].as_str().unwrap().ends_with("+00:00"));
    assert_eq!(
        &out[1..],
        &lines[1..].iter().map(String::as_str).collect::<Vec<_>>()[..]
    );
}

#[test]
fn parses_agent_outputs() {
    let claude =
        "warning: noise\n{\"type\":\"result\",\"session_id\":\"abc-123\",\"result\":\"hi\"}\n";
    assert_eq!(claude_session_id(claude).as_deref(), Some("abc-123"));
    assert_eq!(claude_session_id("no json"), None);
    let codex =
        "{\"type\":\"turn.started\"}\n{\"type\":\"thread.started\",\"thread_id\":\"t-9\"}\n";
    assert_eq!(codex_thread_id(codex).as_deref(), Some("t-9"));
    assert_eq!(codex_thread_id("{}"), None);
}

#[test]
fn claude_rollover_end_to_end_with_fake_cli() {
    let dir = tempfile::tempdir().unwrap();
    let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
    let src = write_claude(
        &homes.claude,
        CLAUDE_ID,
        "/w/p",
        &claude_lines(CLAUDE_ID, "/w/p"),
    );
    let new_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let args_file = dir.path().join("args");
    // The new session lands in a project dir that does NOT match the process
    // cwd: lineage must still be linked.
    let fake = fake_bin(
        dir.path(),
        "claude",
        &format!(
            r#"if [ "$1" = "-p" ]; then
  printf '%s\n' "$@" > '{args}'
  d="$CLAUDE_CONFIG_DIR/projects/-somewhere-else"; mkdir -p "$d"
  echo '{{"type":"user","sessionId":"{new_id}","trim_metadata":{{"parent_file":"/x"}}}}' > "$d/{new_id}.jsonl"
  echo '{{"type":"assistant","sessionId":"{new_id}"}}' >> "$d/{new_id}.jsonl"
  echo 'rc-file noise {{not json'
  echo '{{"type":"result","session_id":"{new_id}"}}'
  echo 'trailing noise'
  exit 0
fi
[ "$1" = "--resume" ] && [ "$2" = "{new_id}" ] && exit 7
exit 99"#,
            args = args_file.display()
        ),
    );
    let bins = AgentBins {
        claude: fake.into(),
        codex: "/nonexistent".into(),
    };
    let req = request(&src, Agent::Claude, RolloverKind::Context, Some("custom"));
    assert_eq!(run(&req, &homes, &bins).unwrap(), ["--resume", new_id]);
    let args = std::fs::read_to_string(&args_file).unwrap();
    assert_eq!(
        args,
        format!("-p\n{}\n--output-format\njson\n", build_prompt(&req))
    );
    let new_file = homes
        .claude
        .join("projects/-somewhere-else")
        .join(format!("{new_id}.jsonl"));
    let first = crate::session::lineage::read_first_line(&new_file).unwrap();
    assert!(first.get("trim_metadata").is_none());
    assert_eq!(first["continue_metadata"]["parent_session_id"], CLAUDE_ID);
    assert_eq!(
        first["continue_metadata"]["parent_session_file"],
        src.display().to_string()
    );
    assert_eq!(full_chain(&new_file)[1].0, src);
}

#[test]
fn codex_rollover_end_to_end_with_fake_cli() {
    let dir = tempfile::tempdir().unwrap();
    let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
    let src = write_codex(
        &homes.codex,
        CODEX_ID,
        "2026-05-07T12-57-30",
        &codex_lines(CODEX_ID, "/w", "cli"),
    );
    let tid = "019e0000-1111-7222-8333-444444444444";
    let args_file = dir.path().join("args");
    let fake = fake_bin(
        dir.path(),
        "codex",
        &format!(
            r#"if [ "$1" = "exec" ]; then
  printf '%s\n' "$@" > '{args}'
  d="$CODEX_HOME/sessions/2026/10/29"; mkdir -p "$d"
  echo '{{"timestamp":"t","type":"session_meta","payload":{{"id":"{tid}"}}}}' > "$d/rollout-2026-10-29T10-00-00-{tid}.jsonl"
  echo '{{"type":"thread.started","thread_id":"{tid}"}}'
  exit 0
fi
[ "$1" = "resume" ] && [ "$2" = "{tid}" ] && exit 0
exit 99"#,
            args = args_file.display()
        ),
    );
    let bins = AgentBins {
        claude: "/nonexistent".into(),
        codex: fake.into(),
    };
    let req = request(&src, Agent::Codex, RolloverKind::Context, None);
    assert_eq!(run(&req, &homes, &bins).unwrap(), ["resume", tid]);
    let args = std::fs::read_to_string(&args_file).unwrap();
    assert_eq!(
        args,
        format!(
            "exec\n--json\n--model\ngpt-5.1-codex-mini\n{}\n",
            build_prompt(&req)
        )
    );
    let new_file = homes.codex.join(format!(
        "sessions/2026/10/29/rollout-2026-10-29T10-00-00-{tid}.jsonl"
    ));
    let first = crate::session::lineage::read_first_line(&new_file).unwrap();
    assert_eq!(first["continue_metadata"]["parent_session_id"], CODEX_ID);

    // Quick rollover: no --model.
    let req = request(&src, Agent::Codex, RolloverKind::Quick, None);
    run(&req, &homes, &bins).unwrap();
    let args = std::fs::read_to_string(&args_file).unwrap();
    assert!(args.starts_with("exec\n--json\n[SESSION LINEAGE]"));
}

#[test]
fn headless_failure_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
    let src = write_claude(
        &homes.claude,
        CLAUDE_ID,
        "/w",
        &claude_lines(CLAUDE_ID, "/w"),
    );
    let fake = fake_bin(dir.path(), "claude", "echo boom >&2; exit 2");
    let bins = AgentBins {
        claude: fake.clone().into(),
        codex: fake.into(),
    };
    let err = run(
        &request(&src, Agent::Claude, RolloverKind::Quick, None),
        &homes,
        &bins,
    )
    .unwrap_err();
    let msg = err.downcast_ref::<Exit>().unwrap().message.clone().unwrap();
    assert!(
        msg.starts_with("❌ Error creating session: claude exited with"),
        "{msg}"
    );
    assert!(msg.contains("stderr: boom"));
    let err = run(
        &request(&src, Agent::Codex, RolloverKind::Quick, None),
        &homes,
        &bins,
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("❌ Error creating session: codex exited with")
    );
}

#[test]
fn never_launches_the_agent_without_a_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
    let src = write_claude(
        &homes.claude,
        CLAUDE_ID,
        "/w",
        &claude_lines(CLAUDE_ID, "/w"),
    );
    let new_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let log = dir.path().join("calls");
    let fake = fake_bin(
        dir.path(),
        "claude",
        &format!(
            r#"echo "$1" >> '{log}'
[ "$1" = "-p" ] && echo '{{"type":"result","session_id":"{new_id}"}}'
exit 0"#,
            log = log.display()
        ),
    );
    let bins = AgentBins {
        claude: fake.into(),
        codex: "/nonexistent".into(),
    };
    let req = request(&src, Agent::Claude, RolloverKind::Quick, None);
    let args = run(&req, &homes, &bins).unwrap();
    // `interactive: false` is what an empty / redirected stdin yields.
    let code = launch(&bins, Agent::Claude, &args, &homes, false).unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "-p\n");
}

#[test]
fn launch_hint_uses_bin_homes_and_model() {
    let homes = Homes::new("/h/claude", "/h/my codex");
    let bins = AgentBins {
        claude: "/bin/fake-claude".into(),
        codex: "/bin/fake-codex".into(),
    };
    let config = Config {
        codex_default_model: "m-1".into(),
        ..Config::default()
    };
    let args = launch_args(Agent::Codex, "t-9", &config, &homes, &bins);
    assert_eq!(args, ["resume", "t-9", "-c", "model=m-1"]);
    assert_eq!(
        launch_hint(&bins, Agent::Codex, &args, &homes),
        "CODEX_HOME='/h/my codex' /bin/fake-codex resume t-9 -c model=m-1"
    );
    let args = launch_args(Agent::Claude, "c-1", &config, &homes, &bins);
    assert_eq!(
        launch_hint(&bins, Agent::Claude, &args, &homes),
        "CLAUDE_CONFIG_DIR=/h/claude /bin/fake-claude --resume c-1"
    );
}
