use std::path::PathBuf;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::cli::GlobalArgs;
use crate::session::testutil::{CLAUDE_ID, CLAUDE_ID2, write_claude};

const TIME: f64 = 1_000_000.0;

struct Fixture {
    _temp: TempDir,
    state: State,
    path: PathBuf,
    original: Vec<u8>,
}

impl Fixture {
    fn new(long: bool) -> Self {
        let temp = TempDir::new().unwrap();
        let global = GlobalArgs {
            claude_home: Some(temp.path().join("claude")),
            codex_home: Some(temp.path().join("codex")),
            pi_home: Some(temp.path().join("pi")),
            opencode_home: Some(temp.path().join("opencode")),
        };
        let homes = global.homes().unwrap();
        let cwd = temp.path().join("project with spaces");
        let lines = [
            json!({
                "type": "user", "sessionId": CLAUDE_ID, "uuid": "user-1",
                "cwd": cwd, "message": {"role": "user", "content": "run a command"}
            }),
            json!({
                "type": "assistant", "sessionId": CLAUDE_ID, "uuid": "assistant-1",
                "message": {"role": "assistant", "content": [{
                    "type": "tool_use", "id": "tool-1", "name": "Bash",
                    "input": {"command": "echo output"}
                }, {"type": "text", "text": "done"}]}
            }),
            json!({
                "type": "user", "sessionId": CLAUDE_ID, "uuid": "user-2",
                "message": {"role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": "tool-1",
                    "content": if long { "x".repeat(20_000) } else { "short".to_owned() }
                }]}
            }),
        ]
        .map(|value| value.to_string());
        let path = write_claude(&homes.claude, CLAUDE_ID, &cwd.display().to_string(), &lines);
        let original = fs::read(&path).unwrap();
        Self {
            state: State::from_cache_dir(Some(temp.path().join("cache"))),
            _temp: temp,
            path,
            original,
        }
    }

    fn prompt(&self, arg: &str, time: f64) -> String {
        handle(
            &self.state,
            CLAUDE_ID,
            self.path.to_str().unwrap(),
            arg,
            time,
            None,
        )
    }

    fn backups(&self) -> Vec<PathBuf> {
        fs::read_dir(self.path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "bak"))
            .collect()
    }
}

#[test]
fn preview_yes_applies_current_session_with_backup_and_same_id() {
    let fixture = Fixture::new(true);
    let message = fixture.prompt("-20 800 Bash,Read", TIME);
    assert!(message.starts_with(&format!(
        "{BLUE}Trim preview - keep last 20 long assistant msgs, threshold 800 chars, tools: bash,read:{RESET}\n"
    )), "{message}");
    assert!(message.contains("tokens would be saved"));
    assert!(message.contains("1 tool results + 0 assistant messages trimmed"));
    assert!(message.contains("Apply:"));
    assert!(!message.contains("Customize"));
    assert_eq!(fs::read(&fixture.path).unwrap(), fixture.original);
    assert!(fixture.backups().is_empty());
    let plan = fixture.state.load(CLAUDE_ID, TIME).unwrap();
    assert_eq!(plan.transcript_path, fixture.path.to_str().unwrap());
    assert_eq!(plan.opts.threshold, Some(800));
    assert_eq!(plan.opts.trim_assistant, Some(-20));
    assert_eq!(plan.opts.tools, ["bash", "read"]);

    let applied = fixture.prompt("YES", TIME + 1.0);
    assert!(
        applied.starts_with(&format!("{GREEN}Session trimmed in place - ")),
        "{applied}"
    );
    assert!(applied.contains(&format!("claude -r {CLAUDE_ID}")));
    assert!(applied.contains("Your current context window is unchanged"));
    let trimmed = fs::read(&fixture.path).unwrap();
    assert!(trimmed.len() < fixture.original.len());
    let backups = fixture.backups();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), fixture.original);
    assert!(applied.contains(&backups[0].display().to_string()));
    for line in String::from_utf8(trimmed).unwrap().lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        assert_eq!(value["sessionId"], CLAUDE_ID);
        assert!(value.get("trim_metadata").is_none());
    }
    assert!(fixture.state.load(CLAUDE_ID, TIME + 1.0).is_none());
    assert!(
        fixture
            .prompt("yes", TIME + 2.0)
            .contains("No pending trim preview")
    );
}

#[test]
fn defaults_preview_has_usage_and_cancel_aliases_abandon_without_writes() {
    let fixture = Fixture::new(true);
    for alias in ["cancel", "NO", "Abort"] {
        let preview = fixture.prompt("", TIME);
        assert!(preview.contains("threshold 500 chars, all tools"));
        assert!(preview.contains("Customize"));
        assert!(fixture.state.load(CLAUDE_ID, TIME).is_some());
        assert_eq!(
            fixture.prompt(alias, TIME + 1.0),
            format!("{YELLOW}Pending trim abandoned. Nothing was changed.{RESET}")
        );
        assert!(fixture.state.load(CLAUDE_ID, TIME + 1.0).is_none());
        assert!(
            fixture
                .prompt("yes", TIME + 2.0)
                .contains("No pending trim preview")
        );
        assert_eq!(fs::read(&fixture.path).unwrap(), fixture.original);
        assert!(fixture.backups().is_empty());
    }
}

#[test]
fn expiry_requires_repreview_before_apply() {
    let fixture = Fixture::new(true);
    assert!(fixture.prompt("", TIME).contains("Trim preview"));
    let expired = fixture.prompt("yes", TIME + 601.0);
    assert_eq!(
        expired,
        format!(
            "{YELLOW}No pending trim preview (or it expired).{RESET}\n\
             {BLUE}Run {CODE}>trim{RESET}{BLUE} or e.g. \
             {CODE}>trim -20{RESET}{BLUE} first.{RESET}"
        )
    );
    assert_eq!(fs::read(&fixture.path).unwrap(), fixture.original);
    assert!(fixture.backups().is_empty());
    assert!(fixture.prompt("800", TIME + 602.0).contains("Trim preview"));
    assert!(
        fixture
            .prompt("go", TIME + 603.0)
            .contains("Session trimmed in place")
    );
}

#[test]
fn changed_transcript_path_disarms_preview_without_touching_either_file() {
    let fixture = Fixture::new(true);
    fixture.prompt("", TIME);
    let other_path = fixture.path.with_file_name("other.jsonl");
    fs::write(&other_path, b"do not touch").unwrap();
    let message = handle(
        &fixture.state,
        CLAUDE_ID,
        other_path.to_str().unwrap(),
        "apply",
        TIME + 1.0,
        None,
    );
    assert!(message.contains("transcript path changed since the preview"));
    assert_eq!(fs::read(&fixture.path).unwrap(), fixture.original);
    assert_eq!(fs::read(&other_path).unwrap(), b"do not touch");
    assert!(fixture.state.load(CLAUDE_ID, TIME + 1.0).is_none());
    assert!(fixture.backups().is_empty());
}

#[test]
fn apply_uses_preview_path_when_hook_has_no_transcript_path() {
    let fixture = Fixture::new(true);
    fixture.prompt("", TIME);
    let message = handle(&fixture.state, CLAUDE_ID, "", "y", TIME + 1.0, None);
    assert!(message.contains("Session trimmed in place"), "{message}");
    assert_eq!(fixture.backups().len(), 1);
}

#[test]
fn help_errors_and_lean_preview_never_arm_or_modify_a_session() {
    let fixture = Fixture::new(false);
    for arg in ["help", "-h", "--help", "?", "HELP"] {
        let message = handle(&fixture.state, CLAUDE_ID, "", arg, TIME, None);
        assert_eq!(message, help_message());
        assert!(message.contains("Two safe steps"));
        assert!(message.contains("NEXT resume"));
    }
    let missing = handle(&fixture.state, CLAUDE_ID, "", "", TIME, None);
    assert_eq!(
        missing,
        format!(
            "{YELLOW}No transcript path available for this session \
             yet - try again after the next message.{RESET}"
        )
    );
    let error = fixture.prompt("500 600", TIME);
    assert!(error.starts_with(&format!(
        "{YELLOW}More than one threshold ('600').{RESET}\n\n"
    )));
    assert!(error.contains("Then"));
    let lean = fixture.prompt("", TIME);
    assert!(lean.contains("Session is already lean - only ~0 tokens would be saved"));
    assert!(lean.contains("Customize"));
    assert!(!fixture.prompt("500", TIME).contains("Customize"));
    assert!(fixture.state.load(CLAUDE_ID, TIME).is_none());
    assert!(!fixture._temp.path().join("cache").exists());
    assert_eq!(fs::read(&fixture.path).unwrap(), fixture.original);
    assert!(fixture.backups().is_empty());
}

#[test]
fn negative_savings_are_rejected() {
    let fixture = Fixture::new(false);
    let result = trim_session_in_place(
        &fixture.path,
        &parse_trim_args("1 +1").unwrap().api_options(),
        true,
    )
    .unwrap();
    assert!(result.stats.chars_saved < 0);
    assert!(!valid_result(&result));
    let message = fixture.prompt("1 +1", TIME);
    assert!(message.starts_with(&format!("{YELLOW}Trim preview failed:{RESET}\n")));
    assert!(message.contains("Trim returned an invalid result: {"));
    assert!(!message.contains("uv tool"));
    assert!(!message.contains("Binary:"));
    assert_eq!(fs::read(&fixture.path).unwrap(), fixture.original);
    assert!(fixture.state.load(CLAUDE_ID, TIME).is_none());
    assert!(fixture.backups().is_empty());
}

#[test]
fn result_counts_and_applied_backup_must_be_credible() {
    let fixture = Fixture::new(true);
    let result =
        trim_session_in_place(&fixture.path, &Options::default().api_options(), true).unwrap();
    assert!(valid_result(&result));
    let mut invalid = result.clone();
    invalid.size_before = MAX_TRUSTED_COUNT + 1;
    assert!(!valid_result(&invalid));
    invalid = result.clone();
    invalid.stats.num_tools_trimmed = MAX_TRUSTED_COUNT + 1;
    assert!(!valid_result(&invalid));
    invalid = result;
    invalid.applied = true;
    assert!(!valid_result(&invalid));
    invalid.backup_file = Some(PathBuf::new());
    assert!(!valid_result(&invalid));
    invalid.backup_file = Some(fixture.path.with_extension("bak"));
    assert!(valid_result(&invalid));
}

#[test]
fn state_write_errors_do_not_arm_and_api_failures_clear_confirmed_plans() {
    let fixture = Fixture::new(true);
    let namespace = fixture._temp.path().join("cache").join("mnemoloom");
    fs::create_dir_all(namespace.parent().unwrap()).unwrap();
    fs::write(&namespace, b"not a directory").unwrap();
    let message = fixture.prompt("", TIME);
    assert!(message.contains("Could not save the trim preview state - nothing was armed"));
    assert_eq!(fs::read(&fixture.path).unwrap(), fixture.original);
    fs::remove_file(namespace).unwrap();
    assert!(fixture.prompt("", TIME).contains("Trim preview"));
    fs::remove_file(&fixture.path).unwrap();
    let message = fixture.prompt("yes", TIME + 1.0);
    assert!(message.starts_with(&format!("{YELLOW}Trim failed:{RESET}\n")));
    assert!(message.contains("Session file not found"));
    assert!(fixture.state.load(CLAUDE_ID, TIME + 1.0).is_none());
}

#[test]
fn session_that_became_lean_after_preview_is_not_rewritten() {
    let fixture = Fixture::new(true);
    fixture.prompt("", TIME);
    let lean = Fixture::new(false);
    fs::write(&fixture.path, &lean.original).unwrap();
    let message = fixture.prompt("apply", TIME + 1.0);
    assert_eq!(
        message,
        format!(
            "{BLUE}Nothing to trim anymore - the session changed \
             since the preview.{RESET}\n\
             {BLUE}(~0 tokens of savings left; file unchanged){RESET}"
        )
    );
    assert_eq!(fs::read(&fixture.path).unwrap(), lean.original);
    assert!(fixture.backups().is_empty());
    assert!(fixture.state.load(CLAUDE_ID, TIME + 1.0).is_none());
}

#[test]
fn timeout_overrides_are_finite_positive_and_capped() {
    assert_eq!(timeout_secs(None), 60.0);
    for invalid in [
        "", " ", "garbage", "0", "-1", "NaN", "inf", "-inf", "_1", "1_", "1__0", "1_.0",
    ] {
        assert_eq!(timeout_secs(Some(invalid)), 60.0);
    }
    assert_eq!(timeout_secs(Some(" 1.5 ")), 1.5);
    assert_eq!(timeout_secs(Some("1_0")), 10.0);
    assert_eq!(timeout_secs(Some("\u{ff11}.\u{ff15}")), 1.5);
    assert_eq!(timeout_secs(Some("1000000")), 75.0);
}

#[test]
fn apply_gets_a_grace_period_beyond_the_timeout() {
    let fixture = Fixture::new(true);
    assert!(fixture.prompt("", TIME).contains("Trim preview"));
    let path = fixture.path.to_str().unwrap();
    let applied = handle(
        &fixture.state,
        CLAUDE_ID,
        path,
        "yes",
        TIME + 1.0,
        Some("1e-9"),
    );
    assert!(applied.contains("Session trimmed in place"), "{applied}");
    assert_eq!(fixture.backups().len(), 1);
}

#[test]
fn transcript_of_another_session_is_refused() {
    let fixture = Fixture::new(true);
    let other = fixture.path.with_file_name(format!("{CLAUDE_ID2}.jsonl"));
    fs::copy(&fixture.path, &other).unwrap();
    for path in [other.to_str().unwrap(), "/tmp/transcript.jsonl"] {
        let message = handle(&fixture.state, CLAUDE_ID, path, "", TIME, None);
        assert_eq!(
            message,
            format!(
                "{YELLOW}Refusing to trim: {path} is not the transcript of \
                 session {CLAUDE_ID}. Nothing was changed.{RESET}"
            )
        );
    }
    assert!(fixture.state.load(CLAUDE_ID, TIME).is_none());
    assert_eq!(fs::read(&other).unwrap(), fixture.original);
    assert!(fixture.backups().is_empty());
}

#[test]
fn worker_budget_returns_promptly_and_reports_errors() {
    assert_eq!(bounded(Duration::from_secs(1), || Ok(123)), Ok(123));
    assert_eq!(
        bounded::<(), _>(Duration::from_secs(1), || Err("failure".to_owned())),
        Err(RunError::Failed("failure".to_owned()))
    );
    let (release, wait) = mpsc::channel();
    let result = bounded(Duration::from_millis(10), move || {
        let _ = wait.recv();
        Ok(())
    });
    assert_eq!(result, Err(RunError::Timeout));
    release.send(()).unwrap();
}

#[test]
fn timeout_diagnostics_quote_commands_and_never_promise_an_apply_was_undone() {
    let opts = parse_trim_args("800 -20 Bash,Read").unwrap();
    let path = "/missing/a 'quoted' transcript.jsonl";
    let preview = timeout_message(path, &opts, 1.5, true);
    assert!(preview.contains("within 1.5s"));
    assert!(preview.contains("This was only a preview, so nothing was changed."));
    assert!(preview.contains("size unknown"));
    assert!(preview.contains("--len 800 --tools bash,read --trim-assistant -20 --dry-run"));
    assert!(preview.contains(&shell_quote(path)));
    assert!(preview.contains("MNEMOLOOM_TRIM_TIMEOUT (seconds, up to 75)"));
    let apply = timeout_message(path, &opts, 25.0, false);
    assert!(apply.contains("is UNKNOWN"));
    assert!(apply.contains("*.pre-trim-*.jsonl.bak"));
    assert!(!apply.contains("nothing was changed"));
    assert!(!apply.contains("--dry-run"));
}

#[test]
fn formats_python_sizes_token_groups_and_significant_seconds() {
    assert_eq!(fmt_size(0), "0 B");
    assert_eq!(fmt_size(1023), "1023 B");
    assert_eq!(fmt_size(1024), "1.0 KB");
    assert_eq!(fmt_size(1024 * 1024), "1.0 MB");
    assert_eq!(group(1234567), "1,234,567");
    assert_eq!(group(-1234), "-1,234");
    for (seconds, formatted) in [
        (25.0, "25"),
        (1.5, "1.5"),
        (25.123456, "25.12"),
        (0.00001, "1e-05"),
        (0.0001, "0.0001"),
        (0.000099999, "0.0001"),
    ] {
        assert_eq!(fmt_secs(seconds), formatted);
    }
}
