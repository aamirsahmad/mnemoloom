//! Bounded subprocess runner: every failure looks like "no output".

use std::process::Command;
use std::time::Duration;

use mnemoloom::process;

/// Timeout for tmux and the process-table snapshot.
pub const SLOW: Duration = Duration::from_secs(10);
/// Timeout for the per-scan probes (lsof, sqlite).
pub const FAST: Duration = Duration::from_secs(5);

/// Run `program args…` with extra `env`, returning stdout on a zero exit.
///
/// Returns `None` on a spawn error, a non-zero exit, or a timeout.
pub fn run(
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
    timeout: Duration,
) -> Option<String> {
    output(program, args, env, timeout).and_then(|(ok, out)| ok.then_some(out))
}

/// Run `program args…`, returning whether it exited zero and its stdout.
///
/// An `env` entry with an empty value removes that variable. Returns `None`
/// on a spawn error or a timeout (the child is killed). stderr is discarded.
fn output(
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
    timeout: Duration,
) -> Option<(bool, String)> {
    let mut command = Command::new(program);
    for &(key, value) in env {
        if value.is_empty() {
            command.env_remove(key);
        } else {
            command.env(key, value);
        }
    }
    let (status, out) = process::run(command.args(args), timeout).ok()?;
    Some((status.success(), out))
}

/// `tmux args…`, with any failure collapsed to an empty string.
pub fn tmux(args: &[&str]) -> String {
    run("tmux", args, &[], SLOW).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout_and_env() {
        let out = run("sh", &["-c", "echo $MUX_T"], &[("MUX_T", "hi")], FAST);
        assert_eq!(out.as_deref(), Some("hi\n"));
    }

    #[test]
    fn output_keeps_stdout_of_failures_and_removes_empty_env() {
        let out = output("sh", &["-c", "echo x; exit 3"], &[], FAST);
        assert_eq!(out, Some((false, "x\n".to_owned())));
        let env = [("MUX_T", "")];
        let unset = output("sh", &["-c", "echo ${MUX_T-unset}"], &env, FAST);
        assert_eq!(unset, Some((true, "unset\n".to_owned())));
    }

    #[test]
    fn failures_are_none() {
        assert_eq!(run("sh", &["-c", "echo x; exit 3"], &[], FAST), None);
        assert_eq!(run("/nonexistent/binary", &[], &[], FAST), None);
    }
}
