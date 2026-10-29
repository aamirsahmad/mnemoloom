//! One `ps` snapshot of the process table, and walks over it.

use std::collections::{HashMap, HashSet, VecDeque};

use super::detect::basename;
use super::exec::{self, SLOW};

/// Safety bound on a process-tree walk, in case of a cycle.
const TREE_LIMIT: usize = 200;
/// Bound on the Codex launcher-to-native chain.
const CODEX_PID_LIMIT: usize = 32;
/// `lstart` is always five fields: weekday, month, day, time, year.
const LSTART_FIELDS: usize = 5;

/// The process table: parentage, command lines, and start identities.
#[derive(Default)]
pub struct Snapshot {
    /// Direct children per ppid, in ps order.
    children: HashMap<u32, Vec<u32>>,
    commands: HashMap<u32, String>,
    /// `lstart` per pid, whitespace-collapsed (`Wed Oct 21 19:56:57 2026`).
    starts: HashMap<u32, String>,
}

/// Snapshot the whole process table in one call. Works on macOS and procps.
///
/// `-ww` keeps command lines whole (procps honours `$COLUMNS` otherwise, and
/// it is removed too). Start times must be in the C locale (Claude records
/// `Wed Oct 21 …`; under e.g. `en_CA` macOS prints `Wed 21 Oct …`) and UTC.
/// `LC_ALL=C` would also escape non-ASCII command lines, so only `LC_TIME` is
/// forced and the user's character set is kept for `LC_CTYPE`.
pub fn snapshot() -> Snapshot {
    let ctype = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|var| std::env::var(var).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();
    let env = [
        ("COLUMNS", ""),
        ("LC_ALL", ""),
        ("LC_CTYPE", &ctype),
        ("LC_TIME", "C"),
        ("TZ", "UTC"),
    ];
    let args = ["-ww", "-eo", "ppid=,pid=,lstart=,command="];
    exec::run("ps", &args, &env, SLOW)
        .map(|out| parse_ps(&out))
        .unwrap_or_default()
}

fn parse_ps(out: &str) -> Snapshot {
    let mut snapshot = Snapshot::default();
    for line in out.lines() {
        let Some((ppid, rest)) = split_field(line.trim()) else {
            continue;
        };
        let Some((pid, mut rest)) = split_field(rest) else {
            continue;
        };
        let (Ok(ppid), Ok(pid)) = (ppid.parse(), pid.parse()) else {
            continue;
        };
        let mut start = Vec::with_capacity(LSTART_FIELDS);
        for _ in 0..LSTART_FIELDS {
            let Some((field, tail)) = split_field(rest) else {
                break;
            };
            start.push(field);
            rest = tail;
        }
        if start.len() < LSTART_FIELDS {
            continue;
        }
        snapshot.starts.insert(pid, start.join(" "));
        snapshot.commands.insert(pid, rest.to_owned());
        snapshot.children.entry(ppid).or_default().push(pid);
    }
    snapshot
}

/// First whitespace-delimited field and the remainder (Python `split(None, 1)`).
fn split_field(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    let end = s.find(char::is_whitespace)?;
    let rest = s[end..].trim_start();
    (!rest.is_empty()).then(|| (&s[..end], rest))
}

impl Snapshot {
    pub fn command(&self, pid: u32) -> Option<&str> {
        self.commands.get(&pid).map(String::as_str)
    }

    pub fn start(&self, pid: u32) -> Option<&str> {
        self.starts.get(&pid).map(String::as_str)
    }

    fn kids(&self, pid: u32) -> impl Iterator<Item = u32> + '_ {
        self.children.get(&pid).into_iter().flatten().copied()
    }

    /// `root` itself, then every process beneath it, breadth-first. The root
    /// counts because `tmux new-window claude` or `sh -c '…; claude'` (which
    /// execs) makes the agent the pane process; descendants find agents under
    /// wrappers (`direnv exec`, a login shell, `just`).
    pub fn process_tree(&self, root: u32) -> Vec<(u32, &str)> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([root]);
        while out.len() < TREE_LIMIT {
            let Some(pid) = queue.pop_front() else {
                break;
            };
            if !seen.insert(pid) {
                continue;
            }
            if let Some(cmd) = self.command(pid) {
                out.push((pid, cmd));
            }
            queue.extend(self.kids(pid));
        }
        out
    }

    /// The agent pid plus its native `codex` children (never tool workers).
    pub fn codex_pids(&self, pid: u32) -> HashSet<u32> {
        let mut pids = HashSet::new();
        if pid == 0 {
            return pids;
        }
        pids.insert(pid);
        let mut queue = VecDeque::from([pid]);
        while pids.len() < CODEX_PID_LIMIT {
            let Some(parent) = queue.pop_front() else {
                break;
            };
            for child in self.kids(parent) {
                let exe = self
                    .command(child)
                    .and_then(|c| c.split_whitespace().next())
                    .map(basename);
                if exe == Some("codex") && pids.insert(child) {
                    queue.push_back(child);
                }
            }
        }
        pids
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    const START: &str = "Wed Oct 21 19:56:57 2026";

    /// Parse `ppid pid command` lines, giving every process the same start.
    pub fn snapshot(lines: &str) -> Snapshot {
        let with_start: Vec<String> = lines
            .lines()
            .map(|l| {
                let mut f = l.split_whitespace();
                let (ppid, pid) = (f.next().unwrap(), f.next().unwrap());
                format!("{ppid} {pid} {START} {}", f.collect::<Vec<_>>().join(" "))
            })
            .collect();
        parse_ps(&with_start.join("\n"))
    }

    const PS: &str = "1 100 -zsh
100 200 direnv exec . claude --resume x
200 300 /home/alice/.local/share/claude/versions/2.1.220 --resume x
300 400 node mcp-server.js
1 500 codex
500 600 /opt/codex/bin/codex --native
600 700 rg codex .
600 800 codex helper";

    fn pids(tree: &[(u32, &str)]) -> Vec<u32> {
        tree.iter().map(|d| d.0).collect()
    }

    #[test]
    fn parses_ps_with_lstart() {
        let out = "    1   100 Sun Sep  6 10:00:00 2026     -zsh -l
  100   200 Wed Oct 21 19:56:57 2026     claude  --resume x
garbage line
    x     9 Wed Oct 21 19:56:57 2026     bad
    1    10 Wed Oct 21
    1    11 Wed Oct 21 19:56:57 2026";
        let snap = parse_ps(out);
        assert_eq!(snap.start(100), Some("Sun Sep 6 10:00:00 2026"));
        assert_eq!(snap.start(200), Some(START));
        assert_eq!(snap.command(100), Some("-zsh -l"));
        assert_eq!(snap.command(200), Some("claude  --resume x"));
        assert_eq!(snap.kids(1).collect::<Vec<_>>(), vec![100]);
        assert_eq!(snap.starts.len(), 2);
    }

    #[test]
    fn parses_procps_output() {
        // procps right-aligns the pid columns and pads the day of month.
        let out =
            "      1     168 Thu Oct 29 05:28:41 2026 tmux new-session -d -s t -c /tmp claude 600
    168     169 Thu Oct  9 05:28:41 2026 claude 600";
        let snap = parse_ps(out);
        assert_eq!(snap.process_tree(169), vec![(169, "claude 600")]);
        assert_eq!(snap.start(169), Some("Thu Oct 9 05:28:41 2026"));
    }

    #[test]
    fn process_tree_is_root_then_breadth_first() {
        let snap = snapshot(PS);
        assert_eq!(pids(&snap.process_tree(100)), vec![100, 200, 300, 400]);
        assert_eq!(snap.kids(1).collect::<Vec<_>>(), vec![100, 500]);
        assert!(snap.process_tree(999).is_empty());
    }

    #[test]
    fn process_tree_includes_an_agent_root() {
        let snap = snapshot("1 42 claude --name x");
        assert_eq!(snap.process_tree(42), vec![(42, "claude --name x")]);
    }

    #[test]
    fn process_tree_survives_cycles() {
        let snap = snapshot("1 2 a\n2 3 b\n3 2 c");
        assert_eq!(pids(&snap.process_tree(1)), vec![2, 3]);
    }

    #[test]
    fn codex_chain_skips_tool_workers() {
        let snap = snapshot(PS);
        let mut pids: Vec<u32> = snap.codex_pids(500).into_iter().collect();
        pids.sort_unstable();
        assert_eq!(pids, vec![500, 600, 800]);
        assert!(snap.codex_pids(0).is_empty());
    }
}
