//! Find every tmux pane running a coding agent (Claude, Codex, pi, opencode).
//!
//! One `tmux list-panes` and one `ps` for the whole server; `capture-pane`
//! only for panes hosting an agent, overlapped with the `lsof` probe.

use super::activity;
use super::detect::{self, classify_argv};
use super::git::GitReader;
use super::model::{Agent, Kind};
use super::procs::{self, Snapshot};
use super::tmux::{self, PaneRecord};

struct Candidate {
    record: PaneRecord,
    kind: Kind,
    /// The agent process and its own command line.
    pid: u32,
    argv: String,
}

/// Live agents sorted by urgency, then pane (plain string order).
pub fn scan(now: f64) -> Vec<Agent> {
    let panes = tmux::list_panes();
    if panes.is_empty() {
        return Vec::new();
    }
    let snapshot = procs::snapshot();
    let candidates: Vec<Candidate> = panes
        .into_iter()
        .filter_map(|p| candidate(&snapshot, p))
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let panes: Vec<&str> = candidates.iter().map(|c| c.record.pane.as_str()).collect();
    let codex: Vec<u32> = candidates
        .iter()
        .filter(|c| c.kind == Kind::Codex)
        .map(|c| c.pid)
        .collect();
    let (screens, open) = rayon::join(
        || tmux::capture_all(&panes),
        || activity::probe_rollouts(&snapshot, &codex),
    );
    let mut agents = assemble_all(candidates, screens);
    activity::enrich(&mut agents, &snapshot, &open, now);
    agents.sort_by(|a, b| (a.state.rank(), &a.pane).cmp(&(b.state.rank(), &b.pane)));
    agents
}

/// Agents for the candidates whose pane still exists. A pane that closed
/// between listing and capture is not a jump target, but a blank screen is
/// kept: an agent that has drawn nothing yet (or cleared it) is still live.
fn assemble_all(candidates: Vec<Candidate>, screens: Vec<Option<String>>) -> Vec<Agent> {
    let mut git = GitReader::default();
    candidates
        .into_iter()
        .zip(screens)
        .filter_map(|(c, screen)| Some(assemble(c, &screen?, &mut git)))
        .collect()
}

fn candidate(snapshot: &Snapshot, record: PaneRecord) -> Option<Candidate> {
    let tree = snapshot.process_tree(record.pid);
    let commands = tree
        .iter()
        .map(|(_, cmd)| *cmd)
        .collect::<Vec<_>>()
        .join("\n");
    let kind = classify_argv(&commands)?;
    // The agent itself, not e.g. a `sleep 600 &` started alongside it.
    let (pid, argv) = tree
        .iter()
        .find(|(_, cmd)| classify_argv(cmd) == Some(kind))
        .or(tree.first())
        .map_or((0, ""), |&(pid, cmd)| (pid, cmd));
    Some(Candidate {
        record,
        kind,
        pid,
        argv: argv.to_owned(),
    })
}

fn assemble(c: Candidate, screen: &str, git: &mut GitReader) -> Agent {
    let PaneRecord {
        pane,
        session,
        title,
        cwd,
        hosts,
        ..
    } = c.record;
    let (repo, branch) = git.context(&cwd);
    let mut agent = Agent::new(pane, session, c.kind);
    agent.state = detect::detect_state(screen, c.kind);
    agent.name = detect::extract_name(&c.argv, &title, screen, &[&hosts[0], &hosts[1]]);
    agent.model = detect::extract_model(screen, c.kind);
    agent.info = detect::extract_info(screen, c.kind);
    agent.pid = c.pid;
    agent.cwd = cwd;
    agent.repo = repo;
    agent.branch = branch;
    agent
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::procs::tests::snapshot;

    fn record(pid: u32) -> PaneRecord {
        PaneRecord {
            pane: "s:1.1".into(),
            session: "s".into(),
            pid,
            title: String::new(),
            cwd: String::new(),
            hosts: Default::default(),
        }
    }

    #[test]
    fn candidate_picks_agent_pid_under_wrappers() {
        let snap = snapshot(
            "1 10 -zsh\n10 11 sleep 600\n10 12 direnv exec . claude\n12 13 claude --name x\n13 14 docker run --name db",
        );
        let c = candidate(&snap, record(10)).unwrap();
        assert_eq!(c.kind, Kind::Claude);
        assert_eq!(c.pid, 13);
        let a = assemble(c, "❯ 1. Yes", &mut GitReader::default());
        assert_eq!(a.name, "x");
    }

    #[test]
    fn name_comes_only_from_the_agent_argv() {
        let snap = snapshot("1 10 -zsh\n10 11 claude\n11 12 docker run --name db");
        let c = candidate(&snap, record(10)).unwrap();
        assert_eq!(c.argv, "claude");
        let a = assemble(c, "─ separator-name ─", &mut GitReader::default());
        assert_eq!(a.name, "separator-name");
    }

    #[test]
    fn agent_as_pane_root_is_found() {
        let snap = snapshot("1 10 claude --resume abc\n10 11 node mcp.js");
        let c = candidate(&snap, record(10)).unwrap();
        assert_eq!((c.kind, c.pid), (Kind::Claude, 10));
        let snap = snapshot("1 20 codex");
        assert_eq!(candidate(&snap, record(20)).unwrap().kind, Kind::Codex);
    }

    #[test]
    fn blank_agent_panes_are_kept_and_closed_panes_dropped() {
        // Linux (tmux 3.5a, procps): `claude 600` as the pane command, and
        // `bash -c 'claude 601; bash'`; neither prints anything.
        let snap = snapshot(
            "1 168 tmux new-session -d -s t claude 600\n168 169 claude 600\n168 171 bash -c claude 601; bash\n171 173 claude 601",
        );
        let candidates: Vec<Candidate> = [169, 171, 169]
            .into_iter()
            .filter_map(|pid| candidate(&snap, record(pid)))
            .collect();
        assert_eq!(
            candidates.iter().map(|c| c.pid).collect::<Vec<_>>(),
            [169, 173, 169]
        );
        let screens = vec![Some(String::new()), Some(String::new()), None];
        let agents = assemble_all(candidates, screens);
        assert_eq!(agents.iter().map(|a| a.pid).collect::<Vec<_>>(), [169, 173]);
        assert!(agents.iter().all(|a| a.kind == Kind::Claude));
    }

    #[test]
    fn pi_and_opencode_panes_are_found() {
        // pi retitles itself to `pi` and starts MCP helpers beneath it.
        let snap = snapshot(
            "1 10 -zsh\n10 11 pi\n11 12 uv tool uvx example-mcp\n11 13 node /opt/bin/npx example-mcp",
        );
        let c = candidate(&snap, record(10)).unwrap();
        assert_eq!((c.kind, c.pid, c.argv.as_str()), (Kind::Pi, 11, "pi"));
        let snap = snapshot("1 20 -zsh\n20 21 /opt/homebrew/bin/opencode -s ses_x");
        let c = candidate(&snap, record(20)).unwrap();
        assert_eq!((c.kind, c.pid), (Kind::OpenCode, 21));
        let a = assemble(c, "esc interrupt", &mut GitReader::default());
        assert_eq!(a.state, crate::mux::model::State::Busy);
        let snap = snapshot("1 30 -zsh\n30 31 pip install pi\n30 32 vim opencode.md");
        assert!(candidate(&snap, record(30)).is_none());
    }

    #[test]
    fn panes_without_agents_are_dropped() {
        let snap = snapshot("1 10 -zsh\n10 11 vim claude.md");
        assert!(candidate(&snap, record(10)).is_none());
        assert!(candidate(&snap, record(99)).is_none());
    }
}
