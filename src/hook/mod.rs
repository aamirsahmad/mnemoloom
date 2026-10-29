//! Claude Code plugin hooks: hook JSON on stdin,
//! `{"decision":"block","reason":...}` on stdout.

use std::io::{self, Read, Write};
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::cli::{GlobalArgs, HookArgs};
use crate::clipboard;

mod trim;
mod trim_args;
mod trim_state;

const RESUME_TRIGGERS: &[&str] = &[">resume", ">continue", ">handoff"];
const SESSION_ID_TRIGGERS: &[&str] = &[">session", ">session-id"];

const BLUE: &str = "\x1b[94m";
const CODE: &str = "\x1b[37m";
const RESET: &str = "\x1b[0m";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    Resume,
    SessionId,
    Trim,
}

/// `mnemoloom hook resume`: UserPromptSubmit handler for `>resume`, `>session`,
/// `>trim`, and the related aliases. Hook failures pass the prompt through.
pub fn resume(_global: &GlobalArgs, _args: &HookArgs) -> anyhow::Result<ExitCode> {
    let mut input = Vec::new();
    if io::stdin().read_to_end(&mut input).is_err() {
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(output) = handle_hook(&input, clipboard::copy) {
        let mut stdout = io::stdout().lock();
        let _ = writeln!(stdout, "{output}");
    }
    Ok(ExitCode::SUCCESS)
}

fn handle_hook<F>(input: &[u8], copy: F) -> Option<String>
where
    F: FnOnce(&str) -> bool,
{
    let value: Value = serde_json::from_slice(input).ok()?;
    let object = value.as_object()?;
    let prompt = object
        .get("prompt")?
        .as_str()?
        .trim_matches(trim_args::is_python_whitespace);
    if prompt.is_empty() {
        return None;
    }

    let trigger = match_trigger(prompt)?;
    let session_id = object
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("");

    let reason = if session_id.is_empty() {
        "No session ID available.".to_owned()
    } else if trigger == Trigger::Trim {
        if !trim_state::is_safe_session_id(session_id) {
            "Session ID looks invalid; not touching trim state.".to_owned()
        } else {
            let transcript_path = object
                .get("transcript_path")
                .and_then(Value::as_str)
                .unwrap_or("");
            trim::handle_trim(session_id, transcript_path, &prompt[">trim".len()..])
        }
    } else {
        format_message(trigger, session_id, copy(session_id))
    };

    serde_json::to_string(&json!({
        "decision": "block",
        "reason": reason,
    }))
    .ok()
}

fn match_trigger(prompt: &str) -> Option<Trigger> {
    let prompt = prompt.to_lowercase();
    if RESUME_TRIGGERS
        .iter()
        .any(|trigger| matches_trigger(&prompt, trigger))
    {
        return Some(Trigger::Resume);
    }
    if SESSION_ID_TRIGGERS
        .iter()
        .any(|trigger| matches_trigger(&prompt, trigger))
    {
        return Some(Trigger::SessionId);
    }
    if matches_trigger(&prompt, ">trim") {
        return Some(Trigger::Trim);
    }
    None
}

fn matches_trigger(prompt: &str, trigger: &str) -> bool {
    prompt == trigger
        || prompt
            .strip_prefix(trigger)
            .is_some_and(|rest| rest.starts_with(' '))
}

fn format_message(trigger: Trigger, session_id: &str, copied: bool) -> String {
    match (trigger, copied) {
        (Trigger::Trim, _) => unreachable!("trim does not use the clipboard"),
        (Trigger::Resume, true) => format!(
            "{BLUE}Session ID copied to clipboard!{RESET}\n\n\
             {BLUE}To continue your work in a new session:{RESET}\n\
             {BLUE}  1. Quit Claude (Ctrl+D twice){RESET}\n\
             {BLUE}  2. Run: {CODE}`mnemoloom resume <paste>`{RESET}\n\n\
             {BLUE}You can then choose between a few different ways of{RESET}\n\
             {BLUE}continuing your work.{RESET}\n\n\
             {BLUE}Session ID: {session_id}{RESET}"
        ),
        (Trigger::Resume, false) => format!(
            "{BLUE}Could not copy to clipboard. Here's your session ID:{RESET}\n\n\
             {BLUE}  {session_id}{RESET}\n\n\
             {BLUE}To continue your work in a new session:{RESET}\n\
             {BLUE}  1. Copy the session ID above{RESET}\n\
             {BLUE}  2. Quit Claude (Ctrl+D twice){RESET}\n\
             {BLUE}  3. Run: {CODE}`mnemoloom resume <session-id>`{RESET}\n\n\
             {BLUE}You can then choose between a few different ways of{RESET}\n\
             {BLUE}continuing your work.{RESET}"
        ),
        (Trigger::SessionId, true) => format!(
            "{BLUE}Session ID copied to clipboard!{RESET}\n\n\
             {BLUE}Session ID: {session_id}{RESET}"
        ),
        (Trigger::SessionId, false) => format!(
            "{BLUE}Could not copy to clipboard. Here's your session ID:{RESET}\n\n\
             {BLUE}  {session_id}{RESET}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output_for(prompt: &str, session_id: &str, copied: bool) -> String {
        let input = json!({
            "prompt": prompt,
            "session_id": session_id,
        })
        .to_string();
        handle_hook(input.as_bytes(), |_| copied).expect("matching hook should produce output")
    }

    #[test]
    fn matches_resume_and_session_triggers() {
        for prompt in [
            ">resume",
            " >RESUME ",
            ">continue now",
            ">handoff\n",
            ">session",
            ">SESSION-ID extra",
        ] {
            let output = output_for(prompt, "abc", true);
            assert!(output.contains("\"decision\":\"block\""));
        }
    }

    #[test]
    fn ignores_non_matching_prompts() {
        for prompt in [
            "hello",
            ">resumex",
            ">session-idfoo",
            ">",
            "",
            "please >resume",
        ] {
            let input = json!({ "prompt": prompt, "session_id": "abc" }).to_string();
            assert!(handle_hook(input.as_bytes(), |_| panic!("must not copy")).is_none());
        }
    }

    #[test]
    fn malformed_input_is_silent() {
        assert!(handle_hook(b"not json", |_| panic!("must not copy")).is_none());
        assert!(
            handle_hook(br#"{"prompt":42,"session_id":"abc"}"#, |_| panic!(
                "must not copy"
            ))
            .is_none()
        );
    }

    #[test]
    fn output_is_block_json_with_resume_instructions() {
        let output = output_for(">resume", "abc", true);
        let value: Value = serde_json::from_str(&output).expect("valid JSON");
        assert_eq!(value["decision"], "block");
        let reason = value["reason"].as_str().expect("reason string");
        assert!(reason.contains("Session ID copied to clipboard!"));
        assert!(reason.contains("`mnemoloom resume <paste>`"));
        assert!(reason.contains("Session ID: abc"));
        assert!(output.starts_with(r#"{"decision":"block","reason":"#));
    }

    #[test]
    fn output_handles_missing_session_id_without_copying() {
        let input = br#"{"prompt":">resume","session_id":""}"#;
        let output = handle_hook(input, |_| panic!("must not copy")).expect("output");
        assert_eq!(
            output,
            r#"{"decision":"block","reason":"No session ID available."}"#
        );
    }

    #[test]
    fn clipboard_failure_changes_resume_message() {
        let output = output_for(">continue", "abc", false);
        let value: Value = serde_json::from_str(&output).expect("valid JSON");
        let reason = value["reason"].as_str().expect("reason string");
        assert!(reason.contains("Could not copy to clipboard."));
        assert!(reason.contains("`mnemoloom resume <session-id>`"));
    }

    #[test]
    fn clipboard_receives_only_the_session_id() {
        let input = br#"{"prompt":">session-id","session_id":"abc"}"#;
        let mut copied_text = None;
        let output = handle_hook(input, |text| {
            copied_text = Some(text.to_owned());
            true
        })
        .expect("output");
        assert_eq!(copied_text.as_deref(), Some("abc"));
        assert!(output.contains("Session ID copied to clipboard!"));
    }

    #[test]
    fn trim_dispatches_without_clipboard_and_preserves_strict_matching() {
        for prompt in [">trim help", " >TRIM --help ", ">trim ?", ">trim HELP"] {
            let input = json!({"prompt": prompt, "session_id": "abc"}).to_string();
            let output = handle_hook(input.as_bytes(), |_| panic!("trim must not copy")).unwrap();
            let value: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(value["decision"], "block");
            assert!(
                value["reason"]
                    .as_str()
                    .unwrap()
                    .contains("CURRENT session")
            );
        }
        for prompt in [">trimming", ">trimyes", ">trim\thelp", ">trim\nhelp"] {
            let input = json!({"prompt": prompt, "session_id": "abc"}).to_string();
            assert!(handle_hook(input.as_bytes(), |_| panic!("must not copy")).is_none());
        }
    }

    #[test]
    fn trim_rejects_hostile_ids_and_missing_or_nonstring_stdin_fields() {
        for id in [json!(""), json!(12), json!(true), Value::Null] {
            let input = json!({"prompt": ">trim help", "session_id": id}).to_string();
            let output = handle_hook(input.as_bytes(), |_| panic!("must not copy")).unwrap();
            let value: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(value["reason"], "No session ID available.");
        }
        for id in ["../escape", "/tmp/escape", "a/b", "a\0b", ".", "-a"] {
            let input = json!({"prompt": ">trim help", "session_id": id}).to_string();
            let output = handle_hook(input.as_bytes(), |_| panic!("must not copy")).unwrap();
            let value: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(
                value["reason"],
                "Session ID looks invalid; not touching trim state."
            );
        }
        let input = br#"{"prompt":">trim","session_id":"abc","transcript_path":42}"#;
        let output = handle_hook(input, |_| panic!("must not copy")).unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert!(
            value["reason"]
                .as_str()
                .unwrap()
                .contains("No transcript path available")
        );
    }
}
