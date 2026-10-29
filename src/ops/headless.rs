//! Running the `claude` / `codex` CLIs: captured headless calls (rollover,
//! smart-trim, query) and final launches that replace `mnemoloom` (`exec`).
//!
//! The binaries are invoked directly. Only when the default name is not
//! found on `PATH` is the command retried through `$SHELL -i -c` (so shell
//! aliases/functions wrapping the CLI still work).
//! `MNEMOLOOM_CLAUDE_BIN` / `MNEMOLOOM_CODEX_BIN` override the binaries (used by tests).
//! Overridden homes are exported (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`) so new
//! sessions land where `mnemoloom` looks for them.
//!
//! Headless children run in their own process group. While `mnemoloom` waits,
//! SIGINT/SIGQUIT only set a flag; the whole group is then killed (as on a
//! timeout), so shell-fallback grandchildren and npm shims die too.

use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Output, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::session::discover::codex_session_paths;
use crate::session::encode::project_dir;
use crate::session::launch::{agent_bin, shell_quote};
use crate::session::resolve::exact_claude_filename;
use crate::session::transcript::WriteStyle;
use crate::session::{self, Agent, Homes, Transcript};

/// Which executables to run for each agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBins {
    pub claude: OsString,
    pub codex: OsString,
}

impl AgentBins {
    /// `MNEMOLOOM_CLAUDE_BIN` / `MNEMOLOOM_CODEX_BIN` (unless empty), else `claude` / `codex`.
    pub fn from_env() -> AgentBins {
        AgentBins {
            claude: agent_bin(Agent::Claude),
            codex: agent_bin(Agent::Codex),
        }
    }

    /// Headless helpers run on Claude or Codex only; pi / opencode map to
    /// Claude.
    pub fn get(&self, agent: Agent) -> &OsStr {
        match agent {
            Agent::Claude | Agent::Pi | Agent::OpenCode => &self.claude,
            Agent::Codex => &self.codex,
        }
    }

    /// True when the binary is the plain default name (shell fallback allowed).
    fn is_default(&self, agent: Agent) -> bool {
        self.get(agent) == agent.as_str()
    }
}

/// Headless (captured, own process group) or final (exec'd, inherits stdio).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Captured,
    Exec,
}

fn build(
    program: &OsStr,
    args: &[String],
    agent: Agent,
    homes: Option<&Homes>,
    mode: Mode,
) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(h) = homes {
        if agent == Agent::Claude && h.claude_overridden {
            cmd.env("CLAUDE_CONFIG_DIR", session::absolute(&h.claude));
        }
        if agent == Agent::Codex && h.codex_overridden {
            cmd.env("CODEX_HOME", session::absolute(&h.codex));
        }
    }
    if mode == Mode::Captured {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
    }
    cmd
}

/// `$SHELL -i -c '<agent> <quoted args>'` (fallback when not on PATH).
fn shell_fallback(agent: Agent, args: &[String], homes: Option<&Homes>, mode: Mode) -> Command {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    let line = std::iter::once(agent.as_str().to_string())
        .chain(args.iter().map(|a| shell_quote(a)))
        .collect::<Vec<_>>()
        .join(" ");
    let shell_args = ["-i".to_string(), "-c".to_string(), line];
    build(&shell, &shell_args, agent, homes, mode)
}

/// Replace `mnemoloom` with the agent (nothing follows a final launch). Returns
/// only on failure.
pub fn exec(bins: &AgentBins, agent: Agent, args: &[String], homes: Option<&Homes>) -> io::Error {
    let err = build(bins.get(agent), args, agent, homes, Mode::Exec).exec();
    if err.kind() == io::ErrorKind::NotFound && bins.is_default(agent) {
        return shell_fallback(agent, args, homes, Mode::Exec).exec();
    }
    err
}

fn spawn_captured(
    bins: &AgentBins,
    agent: Agent,
    args: &[String],
    homes: Option<&Homes>,
) -> io::Result<Child> {
    match build(bins.get(agent), args, agent, homes, Mode::Captured).spawn() {
        Err(e) if e.kind() == io::ErrorKind::NotFound && bins.is_default(agent) => {
            shell_fallback(agent, args, homes, Mode::Captured)
                .spawn()
                .map_err(|_| e)
        }
        other => other,
    }
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
/// Number of active waits and the dispositions they replaced.
static SIGNALS: Mutex<(usize, [libc::sighandler_t; 2])> = Mutex::new((0, [0; 2]));
const WATCHED: [libc::c_int; 2] = [libc::SIGINT, libc::SIGQUIT];

extern "C" fn on_signal(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// While alive, SIGINT/SIGQUIT set [`INTERRUPTED`] instead of killing `mnemoloom`.
struct SignalGuard;

impl SignalGuard {
    fn install() -> SignalGuard {
        let mut s = SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        if s.0 == 0 {
            INTERRUPTED.store(false, Ordering::SeqCst);
            for (i, sig) in WATCHED.into_iter().enumerate() {
                let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                // SAFETY: the handler only stores to an atomic (async-signal-safe).
                s.1[i] = unsafe { libc::signal(sig, handler) };
            }
        }
        s.0 += 1;
        SignalGuard
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        let mut s = SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        s.0 -= 1;
        if s.0 == 0 {
            for (i, sig) in WATCHED.into_iter().enumerate() {
                // SAFETY: restores the disposition saved by `install`.
                unsafe { libc::signal(sig, s.1[i]) };
            }
        }
    }
}

/// SIGKILL the child's whole process group and reap the child.
fn kill_group(child: &mut Child) {
    if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: plain syscall on the group we created (`process_group(0)`).
        unsafe { libc::killpg(pgid, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Run headless with stdin closed, capturing stdout/stderr. A `timeout`
/// kills the process group (`ErrorKind::TimedOut`); so does SIGINT/SIGQUIT
/// (`ErrorKind::Interrupted`, see [`interrupted_exit`]).
pub fn capture(
    bins: &AgentBins,
    agent: Agent,
    args: &[String],
    homes: Option<&Homes>,
    timeout: Option<Duration>,
) -> io::Result<Output> {
    let _signals = SignalGuard::install();
    let mut child = spawn_captured(bins, agent, args, homes)?;
    let readers = [
        child.stdout.take().map(|r| reader(Box::new(r))),
        child.stderr.take().map(|r| reader(Box::new(r))),
    ];
    let deadline = timeout.map(|t| Instant::now() + t);
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if INTERRUPTED.load(Ordering::SeqCst) {
            kill_group(&mut child);
            return Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"));
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            kill_group(&mut child);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "agent CLI timed out",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    let [stdout, stderr] = readers.map(|r| r.and_then(|h| h.join().ok()).unwrap_or_default());
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Exit status for an interrupted headless call (128 + SIGINT).
pub fn interrupted_exit() -> crate::exit::Exit {
    crate::exit::Exit::code(130)
}

fn reader(mut r: Box<dyn Read + Send>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        buf
    })
}

/// Exit code to propagate from a child's status (1 if killed by a signal).
pub fn exit_code(status: std::process::ExitStatus) -> ExitCode {
    ExitCode::from(status.code().map_or(1, |c| (c & 0xff) as u8))
}

/// JSON object lines of `stdout` (shell-fallback noise and other text skipped).
pub fn json_lines(stdout: &str) -> impl Iterator<Item = Value> + '_ {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(Value::is_object)
}

/// The LAST JSON object line of `stdout` (`claude -p --output-format json`).
pub fn last_json_object(stdout: &str) -> Option<Value> {
    json_lines(stdout).last()
}

/// Locate a session file by id: Claude in the process cwd's project dir,
/// else any `projects/*/<id>.jsonl`; Codex by rollout file name.
pub fn find_session_file(agent: Agent, id: &str, homes: &Homes) -> Option<PathBuf> {
    match agent {
        Agent::Claude => {
            let expected = std::env::current_dir().ok().map(|cwd| {
                project_dir(&homes.claude, &cwd.to_string_lossy()).join(format!("{id}.jsonl"))
            });
            expected
                .filter(|p| p.is_file())
                .or_else(|| exact_claude_filename(id, &homes.claude))
        }
        Agent::Codex => codex_session_paths(&homes.codex).into_iter().find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().contains(id))
        }),
        // Never used as headless helpers.
        Agent::Pi | Agent::OpenCode => None,
    }
}

/// A headless helper session (smart-trim / query analysis). On drop it is
/// marked `sessionType: helper` (in case deletion fails) and deleted.
pub struct HelperSession<'a> {
    pub agent: Agent,
    pub id: Option<String>,
    pub homes: &'a Homes,
}

impl Drop for HelperSession<'_> {
    fn drop(&mut self) {
        let Some(file) = self
            .id
            .as_deref()
            .and_then(|id| find_session_file(self.agent, id, self.homes))
        else {
            return;
        };
        if let Ok(mut t) = Transcript::load(&file, self.agent)
            && super::trim::set_first_line_metadata(
                &mut t.lines,
                None,
                "sessionType",
                Value::from("helper"),
            )
        {
            let _ = t.write_atomic(&file, WriteStyle::Python, None);
        }
        let _ = std::fs::remove_file(&file);
    }
}

/// A private scratch directory for files handed to a headless agent:
/// `$TMPDIR/<prefix><random>`, mode 0700, created fresh (never an existing
/// path, so a pre-planted directory or symlink is refused). Removed with
/// its contents on drop.
pub struct ScratchDir(PathBuf);

impl ScratchDir {
    pub fn new(prefix: &str) -> io::Result<ScratchDir> {
        let name = format!("{prefix}{}", uuid::Uuid::new_v4().simple());
        let path = std::env::temp_dir().join(name);
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(ScratchDir(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Create `name` in the directory (exclusively, mode 0600) holding `bytes`.
    pub fn write(&self, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
        let path = self.0.join(name);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?
            .write_all(bytes)?;
        Ok(path)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn capture_output_and_status() {
        let dir = tempfile::tempdir().unwrap();
        let fake = script(dir.path(), "fake", "echo \"out:$1\"\necho err >&2\nexit 3");
        let bins = AgentBins {
            claude: fake.clone().into(),
            codex: fake.into(),
        };
        let out = capture(&bins, Agent::Claude, &["hi".into()], None, None).unwrap();
        assert_eq!(out.stdout, b"out:hi\n");
        assert_eq!(out.stderr, b"err\n");
        assert_eq!(exit_code(out.status), ExitCode::from(3));
    }

    #[test]
    fn timeout_kills_the_whole_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild.pid");
        // The grandchild keeps the pipes open; killing only the direct child
        // would leave it running.
        let fake = script(
            dir.path(),
            "slow",
            &format!("sleep 30 &\necho $! > '{}'\nwait", pidfile.display()),
        );
        let bins = AgentBins {
            claude: fake.clone().into(),
            codex: fake.into(),
        };
        // Under a loaded parallel test run the script may not reach the fork
        // before the timeout; retry until a grandchild actually existed.
        let pid = (0..5)
            .find_map(|_| {
                let started = Instant::now();
                let err = capture(&bins, Agent::Codex, &[], None, Some(Duration::from_secs(2)))
                    .unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::TimedOut);
                assert!(started.elapsed() < Duration::from_secs(10));
                std::fs::read_to_string(&pidfile)
                    .ok()?
                    .trim()
                    .parse::<libc::pid_t>()
                    .ok()
            })
            .expect("grandchild never started");
        thread::sleep(Duration::from_millis(100));
        // SAFETY: signal 0 only probes for existence.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "grandchild {pid} survived the timeout");
    }

    #[test]
    fn missing_override_is_not_found() {
        let bins = AgentBins {
            claude: "/nonexistent/claude-bin".into(),
            codex: "/nonexistent/codex-bin".into(),
        };
        let err = capture(&bins, Agent::Claude, &[], None, None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn scratch_dir_is_private_unique_and_removed() {
        let a = ScratchDir::new("mnemoloom-test-").unwrap();
        let b = ScratchDir::new("mnemoloom-test-").unwrap();
        assert_ne!(a.path(), b.path());
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(a.path()), 0o700);
        let f = a.write("x.txt", b"hi").unwrap();
        assert_eq!(mode(&f), 0o600);
        assert_eq!(fs::read(&f).unwrap(), b"hi");
        assert!(a.write("x.txt", b"again").is_err(), "existing file reused");
        let dir = a.path().to_path_buf();
        drop(a);
        assert!(!dir.exists());
    }

    #[test]
    fn json_object_lines_skip_noise() {
        let out = "rc: hello\n{\"a\":1}\n[1]\n  {\"session_id\":\"x\"}  \nbye\n";
        assert_eq!(json_lines(out).count(), 2);
        assert_eq!(last_json_object(out).unwrap()["session_id"], "x");
        assert!(last_json_object("noise only").is_none());
    }

    #[test]
    fn helper_session_is_deleted_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path().join("claude"), dir.path().join("codex"));
        let c = write_claude(
            &homes.claude,
            CLAUDE_ID,
            "/elsewhere",
            &claude_lines(CLAUDE_ID, "/x"),
        );
        let x = write_codex(
            &homes.codex,
            CODEX_ID,
            "2026-01-02T12-57-30",
            &codex_lines(CODEX_ID, "/w", "exec"),
        );
        drop(HelperSession {
            agent: Agent::Claude,
            id: Some(CLAUDE_ID.into()),
            homes: &homes,
        });
        drop(HelperSession {
            agent: Agent::Codex,
            id: Some(CODEX_ID.into()),
            homes: &homes,
        });
        drop(HelperSession {
            agent: Agent::Codex,
            id: None,
            homes: &homes,
        });
        assert!(!c.exists() && !x.exists());
    }
}
