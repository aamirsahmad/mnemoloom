//! The two-step `>trim` workflow. Only an explicit confirmation writes a session.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::commands::thousands as group;
use crate::ops::trim::{InPlaceResult, trim_session_in_place};
use crate::session::launch::shell_quote;
use crate::session::pyjson;

use super::trim_args::{Options, is_python_whitespace, parse_python_float, parse_trim_args};
use super::trim_state::{Plan, State, now};
use super::{BLUE, CODE, RESET};

const GREEN: &str = "\x1b[92m";
const YELLOW: &str = "\x1b[93m";
const TIMEOUT_ENV: &str = "MNEMOLOOM_TRIM_TIMEOUT";
const TIMEOUT_SECS: f64 = 60.0;
/// Upper bound for `MNEMOLOOM_TRIM_TIMEOUT`. Plus [`APPLY_GRACE_SECS`] it stays
/// under the 90 s `hooks.json` timeout.
const TIMEOUT_HARD_CAP_SECS: f64 = 75.0;
/// Extra wait before an apply is abandoned: the worker dies with the
/// process, so let a splice/rename/drain that is under way finish.
const APPLY_GRACE_SECS: f64 = 10.0;
const _: () = assert!(TIMEOUT_HARD_CAP_SECS + APPLY_GRACE_SECS < 90.0);
const MAX_TRUSTED_COUNT: u64 = 1_000_000_000_000_000;

pub(super) fn handle_trim(session_id: &str, transcript_path: &str, raw_arg: &str) -> String {
    let timeout_override = std::env::var(TIMEOUT_ENV).ok();
    handle(
        &State::default(),
        session_id,
        transcript_path,
        raw_arg,
        now(),
        timeout_override.as_deref(),
    )
}

fn handle(
    state: &State,
    session_id: &str,
    transcript_path: &str,
    raw_arg: &str,
    now: f64,
    timeout_override: Option<&str>,
) -> String {
    let arg = raw_arg.trim_matches(is_python_whitespace);
    match arg.to_lowercase().as_str() {
        "help" | "-h" | "--help" | "?" => help_message(),
        "cancel" | "no" | "abort" => {
            state.clear(session_id);
            format!("{YELLOW}Pending trim abandoned. Nothing was changed.{RESET}")
        }
        "yes" | "y" | "apply" | "go" => {
            apply(state, session_id, transcript_path, now, timeout_override)
        }
        _ => preview(
            state,
            session_id,
            transcript_path,
            arg,
            now,
            timeout_override,
        ),
    }
}

fn apply(
    state: &State,
    session_id: &str,
    transcript_path: &str,
    now: f64,
    timeout_override: Option<&str>,
) -> String {
    let Some(plan) = state.load(session_id, now) else {
        return format!(
            "{YELLOW}No pending trim preview (or it expired).{RESET}\n\
             {BLUE}Run {CODE}>trim{RESET}{BLUE} or e.g. \
             {CODE}>trim -20{RESET}{BLUE} first.{RESET}"
        );
    };
    if !transcript_path.is_empty() && transcript_path != plan.transcript_path {
        state.clear(session_id);
        return format!(
            "{YELLOW}This session's transcript path changed since \
             the preview - nothing was trimmed.{RESET}\n\
             {BLUE}Run {CODE}>trim{RESET}{BLUE} again for a fresh \
             preview.{RESET}"
        );
    }
    let result = run_trim(&plan.transcript_path, &plan.opts, false, timeout_override);
    state.clear(session_id);
    let result = match result {
        Ok(result) => result,
        Err(error) => return format!("{YELLOW}Trim failed:{RESET}\n{BLUE}{error}{RESET}"),
    };
    let tokens = group(result.stats.tokens_saved());
    if result.nothing_to_trim {
        return format!(
            "{BLUE}Nothing to trim anymore - the session changed \
             since the preview.{RESET}\n\
             {BLUE}(~{tokens} tokens of \
             savings left; file unchanged){RESET}"
        );
    }
    let backup = result
        .backup_file
        .as_ref()
        .map_or_else(|| "None".to_owned(), |path| path.display().to_string());
    format!(
        "{GREEN}Session trimmed in place - ~{tokens} tokens saved!{RESET}\n\
         {BLUE}  File size: {} -> {}{RESET}\n\
         {BLUE}  Backup: {backup}{RESET}\n\n\
         {BLUE}Your current context window is unchanged; the savings \
         take effect{RESET}\n\
         {BLUE}next time this session is resumed (quit, then \
         {CODE}claude -r {session_id}{RESET}{BLUE}).{RESET}",
        fmt_size(result.size_before),
        fmt_size(result.size_after),
    )
}

fn preview(
    state: &State,
    session_id: &str,
    transcript_path: &str,
    arg: &str,
    now: f64,
    timeout_override: Option<&str>,
) -> String {
    let started = Instant::now();
    if transcript_path.is_empty() {
        return format!(
            "{YELLOW}No transcript path available for this session \
             yet - try again after the next message.{RESET}"
        );
    }
    if Path::new(transcript_path).file_stem() != Some(OsStr::new(session_id)) {
        return format!(
            "{YELLOW}Refusing to trim: {transcript_path} is not the \
             transcript of session {session_id}. Nothing was changed.{RESET}"
        );
    }
    let opts = match parse_trim_args(arg) {
        Ok(opts) => opts,
        Err(error) => {
            return format!(
                "{YELLOW}{error}{RESET}\n\n{}\n\
                 {BLUE}Then {CODE}>trim yes{RESET}{BLUE} to apply, \
                 {CODE}>trim cancel{RESET}{BLUE} to abandon.{RESET}",
                usage_lines(),
            );
        }
    };
    let result = match run_trim(transcript_path, &opts, true, timeout_override) {
        Ok(result) => result,
        Err(error) => return format!("{YELLOW}Trim preview failed:{RESET}\n{BLUE}{error}{RESET}"),
    };
    let tokens = group(result.stats.tokens_saved());
    let usage = if arg.is_empty() {
        format!("\n\n{}", usage_lines())
    } else {
        String::new()
    };
    if result.nothing_to_trim {
        return format!(
            "{BLUE}Session is already lean - only \
             ~{tokens} tokens would be saved ({}).{RESET}\n\
             {BLUE}Nothing to do.{RESET}{usage}",
            opts.describe(),
        );
    }
    let plan = Plan {
        created_at: now + started.elapsed().as_secs_f64(),
        transcript_path: transcript_path.to_owned(),
        opts: opts.clone(),
        preview_tokens: Some(result.stats.tokens_saved().into()),
    };
    if let Err(error) = state.save(session_id, &plan) {
        return format!(
            "{YELLOW}Could not save the trim preview state - nothing \
             was armed.{RESET}\n{BLUE}{error}{RESET}"
        );
    }
    format!(
        "{BLUE}Trim preview - {}:{RESET}\n\
         {GREEN}  ~{tokens} tokens would be saved ({} chars){RESET}\n\
         {BLUE}  {} tool results + {} assistant messages trimmed{RESET}\n\
         {BLUE}  File size: {} -> {}{RESET}\n\n\
         {BLUE}Apply:  {CODE}>trim yes{RESET}    \
         {BLUE}Cancel:  {CODE}>trim cancel{RESET}{usage}\n\n\
         {BLUE}(Trims this session's file in place; a timestamped \
         .bak backup is kept.{RESET}\n\
         {BLUE}Savings apply from the next resume of this session.){RESET}",
        opts.describe(),
        group(result.stats.chars_saved),
        result.stats.num_tools_trimmed,
        result.stats.num_assistant_trimmed,
        fmt_size(result.size_before),
        fmt_size(result.size_after),
    )
}

fn run_trim(
    transcript_path: &str,
    opts: &Options,
    dry_run: bool,
    timeout_override: Option<&str>,
) -> Result<InPlaceResult, String> {
    let mut seconds = timeout_secs(timeout_override);
    if !dry_run {
        seconds += APPLY_GRACE_SECS;
    }
    let path = Path::new(transcript_path).to_path_buf();
    let api_opts = opts.api_options();
    let result = bounded(Duration::from_secs_f64(seconds), move || {
        trim_session_in_place(&path, &api_opts, dry_run).map_err(|e| format!("{e:#}"))
    });
    match result {
        Err(RunError::Timeout) => Err(timeout_message(transcript_path, opts, seconds, dry_run)),
        Err(RunError::Failed(error)) => Err(error),
        Ok(result) if valid_result(&result) => Ok(result),
        Ok(result) => {
            let detail: String = pyjson::dumps(&result.to_json()).chars().take(400).collect();
            Err(format!("Trim returned an invalid result: {detail}"))
        }
    }
}

fn valid_result(result: &InPlaceResult) -> bool {
    // The typed API guarantees field types; keep the magnitude and
    // non-negative-count checks (a tiny threshold can yield negative savings).
    [
        result.stats.num_tools_trimmed,
        result.stats.num_assistant_trimmed,
        result.size_before,
        result.size_after,
    ]
    .iter()
    .all(|n| *n <= MAX_TRUSTED_COUNT)
        && [result.stats.chars_saved, result.stats.tokens_saved()]
            .iter()
            .all(|n| u64::try_from(*n).is_ok_and(|n| n <= MAX_TRUSTED_COUNT))
        && (!result.applied
            || result
                .backup_file
                .as_ref()
                .is_some_and(|path| !path.as_os_str().is_empty()))
}

#[derive(Debug, PartialEq, Eq)]
enum RunError {
    Timeout,
    Failed(String),
}

fn bounded<T, F>(budget: Duration, work: F) -> Result<T, RunError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("hook-trim".to_owned())
        .spawn(move || {
            let _ = sender.send(work());
        })
        .map_err(|e| RunError::Failed(format!("Cannot run the trim command ({e}).")))?;
    // The API has no cancellation token. On timeout the one-shot mnemoloom hook
    // reports immediately and exits, terminating its worker too. An apply
    // may already have swapped the file; never promise otherwise.
    match receiver.recv_timeout(budget) {
        Ok(result) => result.map_err(RunError::Failed),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(RunError::Timeout),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(RunError::Failed(
            "Trim command failed to run (trim worker stopped without a result).".to_owned(),
        )),
    }
}

/// `MNEMOLOOM_TRIM_TIMEOUT` when finite and positive (capped), else the default.
fn timeout_secs(override_value: Option<&str>) -> f64 {
    override_value
        .and_then(parse_python_float)
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map_or(TIMEOUT_SECS, |seconds| seconds.min(TIMEOUT_HARD_CAP_SECS))
}

fn timeout_message(transcript_path: &str, opts: &Options, seconds: f64, dry_run: bool) -> String {
    let mut command = vec![
        "mnemoloom".to_owned(),
        "trim-in-place".to_owned(),
        transcript_path.to_owned(),
        "--json".to_owned(),
        "--len".to_owned(),
        opts.api_options().threshold.to_string(),
    ];
    if !opts.tools.is_empty() {
        command.extend(["--tools".to_owned(), opts.tools.join(",")]);
    }
    if let Some(n) = opts.trim_assistant {
        command.extend(["--trim-assistant".to_owned(), n.to_string()]);
    }
    if dry_run {
        command.push("--dry-run".to_owned());
    }
    let quoted = command
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let size = fs::metadata(transcript_path)
        .map(|m| fmt_size(m.len()))
        .unwrap_or_else(|_| "size unknown".to_owned());
    let binary = binary_path();
    let status = if dry_run {
        "This was only a preview, so nothing was changed."
    } else {
        "It was applying the trim, so whether the file was rewritten \
         is UNKNOWN. Check the transcript and any \
         *.pre-trim-*.jsonl.bak backup beside it before retrying."
    };
    format!(
        "The trim CLI did not finish within {}s, so it was stopped.\n\
         {status}\n  transcript: {transcript_path} ({size})\n  binary:     {binary}\n\n\
         Run the same command yourself to see the real failure:\n  {quoted}\n\n\
         If that returns quickly, the trim was starved rather than slow \
         - just retry >trim.\n\
         To allow more time, set {TIMEOUT_ENV} (seconds, up to \
         {}) before starting Claude.",
        fmt_secs(seconds),
        fmt_secs(TIMEOUT_HARD_CAP_SECS),
    )
}

fn binary_path() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "mnemoloom".to_owned())
}

fn fmt_size(size: u64) -> String {
    if size >= 1024 * 1024 {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    } else if size >= 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else {
        format!("{size} B")
    }
}

fn fmt_secs(value: f64) -> String {
    let scientific = format!("{value:.3e}");
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return scientific;
    };
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if (-4..4).contains(&exponent) {
        let precision = usize::try_from((3 - exponent).max(0)).unwrap_or(0);
        let fixed = format!("{value:.precision$}");
        if fixed.contains('.') {
            fixed.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            fixed
        }
    } else {
        format!(
            "{}e{}{exponent:02}",
            mantissa.trim_end_matches('0').trim_end_matches('.'),
            if exponent < 0 { '-' } else { '+' },
            exponent = exponent.unsigned_abs(),
        )
    }
}

fn usage_lines() -> String {
    format!(
        "{BLUE}Customize (tokens in any order: -N/+N = assistant msgs,{RESET}\n\
         {BLUE}number = char threshold, words = tool names):{RESET}\n\
         {CODE}  >trim -20            {RESET}\
         {BLUE}keep last 20 long assistant msgs{RESET}\n\
         {CODE}  >trim -20 800        {RESET}\
         {BLUE}... with threshold 800 chars{RESET}\n\
         {CODE}  >trim -20 bash,read  {RESET}\
         {BLUE}... trim only Bash/Read results{RESET}"
    )
}

fn help_message() -> String {
    format!(
        "{BLUE}>trim - trim the CURRENT session's file in place to save tokens.{RESET}\n\n\
         {BLUE}Two safe steps - a preview never changes anything; only \
         {CODE}>trim yes{RESET}{BLUE} writes:{RESET}\n\
         {CODE}  >trim              {RESET}\
         {BLUE}preview savings with defaults (threshold 500, all tools){RESET}\n\
         {CODE}  >trim yes          {RESET}\
         {BLUE}apply the previewed trim (expires ~10 min){RESET}\n\
         {CODE}  >trim cancel       {RESET}\
         {BLUE}abandon the pending preview{RESET}\n\n{}\n\n\
         {BLUE}Same session ID is kept; a timestamped .bak backup is saved next to{RESET}\n\
         {BLUE}the session file. Savings apply on the NEXT resume of this session,{RESET}\n\
         {BLUE}not the current context window.{RESET}",
        usage_lines(),
    )
}

#[cfg(test)]
#[path = "trim_tests.rs"]
mod tests;
