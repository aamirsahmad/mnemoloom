//! Live-session detection: running `claude` / `codex`
//! processes, their cwds, and the N most recently written sessions of that
//! agent in each cwd. Keyed by session file path (sub-agent siblings share
//! ids, paths are unique).

use std::collections::HashMap;
use std::fs;
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use super::query::Row;
use crate::process;
use crate::session::Agent;

/// Process state of a live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveState {
    Running,
    Waiting,
}

/// Timeout for the `ps` snapshot.
const PS_TIMEOUT: Duration = Duration::from_secs(10);

/// Session file path -> state.
pub type LiveMap = HashMap<String, LiveState>;

/// One agent process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub agent: Agent,
    pub running: bool,
}

/// Agent processes from `ps` (stopped ones skipped).
fn agent_procs() -> Vec<Proc> {
    let mut ps = Command::new("ps");
    ps.args(["-ww", "-eo", "pid=,stat=,comm="])
        .env("LC_ALL", "C");
    process::run(&mut ps, PS_TIMEOUT)
        .map(|(_, out)| out.lines().filter_map(parse_ps_line).collect())
        .unwrap_or_default()
}

/// `pid stat comm` -> agent process, if it is one.
pub fn parse_ps_line(line: &str) -> Option<Proc> {
    let mut it = line.split_whitespace();
    let (pid, stat) = (it.next()?, it.next()?);
    let comm = it.collect::<Vec<_>>().join(" ");
    let agent = if comm == "claude" || comm.ends_with("/claude") {
        Agent::Claude
    } else if comm == "codex" || comm.contains("/codex/codex") || comm.ends_with("/codex") {
        Agent::Codex
    } else {
        return None;
    };
    if stat.starts_with('T') {
        return None;
    }
    Some(Proc {
        pid: pid.parse().ok()?,
        agent,
        running: stat.starts_with('R'),
    })
}

/// pid -> cwd (`/proc` on Linux, one `lsof` call elsewhere).
fn proc_cwds(pids: &[u32]) -> HashMap<u32, String> {
    if cfg!(target_os = "linux") {
        return pids
            .iter()
            .filter_map(|&p| {
                let cwd = fs::read_link(format!("/proc/{p}/cwd")).ok()?;
                Some((p, cwd.to_string_lossy().into_owned()))
            })
            .collect();
    }
    if pids.is_empty() {
        return HashMap::new();
    }
    let Ok((_, out)) = process::lsof(pids, &["-d", "cwd"]) else {
        return HashMap::new();
    };
    process::lsof_names(&out)
        .map(|(pid, cwd)| (pid, cwd.to_owned()))
        .collect()
}

/// The part of a row live detection needs (sub-agents excluded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub agent: Agent,
    pub cwd: String,
    pub file_path: String,
}

/// Live-detection candidates from catalog rows.
pub fn candidates(rows: &[Row]) -> Vec<Candidate> {
    rows.iter()
        .filter(|r| !r.is_sidechain)
        .map(|r| Candidate {
            agent: r.agent,
            cwd: r.cwd.clone(),
            file_path: r.file_path.clone(),
        })
        .collect()
}

/// Scan processes and map them onto the candidates.
pub fn scan(rows: &[Candidate]) -> LiveMap {
    let procs = agent_procs();
    let pids: Vec<u32> = procs.iter().map(|p| p.pid).collect();
    let cwds = proc_cwds(&pids);
    let mut groups: HashMap<(Agent, String), (usize, bool)> = HashMap::new();
    for p in &procs {
        let Some(cwd) = cwds.get(&p.pid).filter(|c| c.as_str() != "/") else {
            continue;
        };
        let g = groups.entry((p.agent, cwd.clone())).or_default();
        g.0 += 1;
        g.1 |= p.running;
    }
    assign(&groups, rows)
}

/// For each (agent, cwd) group of `count` processes, mark the `count` most
/// recently modified (file mtime) non-sub-agent sessions there.
pub fn assign(groups: &HashMap<(Agent, String), (usize, bool)>, rows: &[Candidate]) -> LiveMap {
    let mut live = LiveMap::new();
    for ((agent, cwd), &(count, running)) in groups {
        let mut cands: Vec<(u128, &Candidate)> = rows
            .iter()
            .filter(|r| r.agent == *agent && r.cwd == *cwd)
            .map(|r| (mtime_ns(&r.file_path), r))
            .collect();
        cands.sort_by_key(|c| std::cmp::Reverse(c.0));
        let state = if running {
            LiveState::Running
        } else {
            LiveState::Waiting
        };
        for (_, r) in cands.into_iter().take(count) {
            live.insert(r.file_path.clone(), state);
        }
    }
    live
}

fn mtime_ns(path: &str) -> u128 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_parsing() {
        assert_eq!(
            parse_ps_line(" 123 S+ /opt/homebrew/bin/claude"),
            Some(Proc {
                pid: 123,
                agent: Agent::Claude,
                running: false
            })
        );
        assert_eq!(parse_ps_line("9 R codex").map(|p| p.running), Some(true));
        assert_eq!(parse_ps_line("9 T claude"), None);
        assert_eq!(parse_ps_line("9 S claude-helper"), None);
    }
}
