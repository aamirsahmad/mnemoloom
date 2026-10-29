//! Bounded subprocesses: every child is killed when its timeout expires.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Timeout for `lsof`, which can block on stale network mounts.
const LSOF_TIMEOUT: Duration = Duration::from_secs(5);
/// How often `wait` checks for exit once stdout is drained.
const POLL: Duration = Duration::from_millis(5);

/// Run `cmd` with stdin from `/dev/null` and stderr discarded, returning its
/// exit status and stdout.
pub fn run(cmd: &mut Command, timeout: Duration) -> io::Result<(ExitStatus, String)> {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    wait(child, timeout)
}

/// Drain `child`'s stdout (if piped) and wait for it to exit; on a timeout
/// or read error it is killed and reaped.
pub fn wait(mut child: Child, timeout: Duration) -> io::Result<(ExitStatus, String)> {
    let deadline = Instant::now() + timeout;
    let stdout = child.stdout.take().map(|mut out| {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = tx.send(out.read_to_end(&mut buf).map(|_| buf));
        });
        rx
    });
    let buf = match stdout {
        Some(rx) => rx
            .recv_timeout(timeout)
            .unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into())),
        None => Ok(Vec::new()),
    };
    let result = buf.and_then(|buf| {
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok((status, String::from_utf8_lossy(&buf).into_owned()));
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            thread::sleep(POLL);
        }
    });
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

/// `lsof -nP -a <filter…> -p <pids> -Fn` in the C locale. lsof exits
/// non-zero when any listed pid has gone, yet still reports the live ones.
pub fn lsof(pids: &[u32], filter: &[&str]) -> io::Result<(ExitStatus, String)> {
    let mut sorted = pids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let list: Vec<String> = sorted.iter().map(u32::to_string).collect();
    let mut cmd = Command::new("lsof");
    cmd.args(["-nP", "-a"])
        .args(filter)
        .args(["-p", &list.join(","), "-Fn"])
        .env("TZ", "UTC")
        .env("LC_ALL", "C");
    run(&mut cmd, LSOF_TIMEOUT)
}

/// `(pid, name)` for each `n<name>` line of `lsof -F` output, attributed to
/// the preceding `p<pid>` line (0 before any).
pub fn lsof_names(out: &str) -> impl Iterator<Item = (u32, &str)> {
    let mut pid = 0;
    out.lines().filter_map(move |line| {
        if let Some(p) = line.strip_prefix('p').and_then(|p| p.parse().ok()) {
            pid = p;
            None
        } else {
            line.strip_prefix('n').map(|name| (pid, name))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_status_and_stdout() {
        let (status, out) = run(
            Command::new("sh").args(["-c", "echo x; exit 3"]),
            LSOF_TIMEOUT,
        )
        .unwrap();
        assert_eq!((status.code(), out.as_str()), (Some(3), "x\n"));
        assert!(run(&mut Command::new("/nonexistent/binary"), LSOF_TIMEOUT).is_err());
    }

    #[test]
    fn timeout_kills_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let script = format!("echo $$ > {}; exec sleep 5", pidfile.display());
        let start = Instant::now();
        let err = run(
            Command::new("sh").args(["-c", &script]),
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(2));
        let pid = std::fs::read_to_string(&pidfile).unwrap();
        let alive = Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!alive.success());
    }

    #[test]
    fn timeout_applies_without_stdout() {
        let child = Command::new("sleep")
            .arg("5")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let err = wait(child, Duration::from_millis(50)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn lsof_field_parsing() {
        let out = "n/orphan\np100\nfcwd\nn/a/b\nn/c\npx\nn/d\np200\nn/e\n";
        let names: Vec<(u32, &str)> = lsof_names(out).collect();
        assert_eq!(
            names,
            [
                (0, "/orphan"),
                (100, "/a/b"),
                (100, "/c"),
                (100, "/d"),
                (200, "/e")
            ]
        );
    }
}
