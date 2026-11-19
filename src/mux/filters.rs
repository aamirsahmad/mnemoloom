//! Display-time views: repository/directory selection, dormancy, sort order.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::git::repository_name;
use super::model::{Agent, Inactivity};
use super::paths::resolve;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum SortKey {
    /// Most recent known submitted input first, unknown last.
    #[default]
    Newest,
    /// Current state (urgency), then most recent known submitted input.
    State,
    /// Oldest known submitted input first, unknown last.
    Oldest,
}

/// Everything that shapes which rows are shown and in what order.
#[derive(Clone, Debug)]
pub struct View {
    pub repo: Option<String>,
    pub dir: Option<PathBuf>,
    pub dormant: bool,
    pub dormant_hours: f64,
    pub sort: SortKey,
}

/// Sort `agents` according to `sort`.
pub fn sort_agents(agents: &mut [Agent], sort: SortKey) {
    let newest_cmp = |a: &Agent, b: &Agent| {
        a.last_input_at
            .is_none()
            .cmp(&b.last_input_at.is_none())
            .then_with(|| {
                b.last_input_at
                    .unwrap_or(0.0)
                    .total_cmp(&a.last_input_at.unwrap_or(0.0))
            })
    };
    match sort {
        SortKey::Newest => {
            agents.sort_by(|a, b| {
                newest_cmp(a, b)
                    .then_with(|| (a.state.rank(), &a.pane).cmp(&(b.state.rank(), &b.pane)))
            });
        }
        SortKey::State => {
            agents.sort_by(|a, b| {
                a.state
                    .rank()
                    .cmp(&b.state.rank())
                    .then_with(|| newest_cmp(a, b))
                    .then_with(|| a.pane.cmp(&b.pane))
            });
        }
        SortKey::Oldest => {
            agents.sort_by(|a, b| {
                let key = |x: &Agent| (x.last_input_at.is_none(), x.last_input_at.unwrap_or(0.0));
                let (ka, kb) = (key(a), key(b));
                ka.0.cmp(&kb.0)
                    .then(ka.1.total_cmp(&kb.1))
                    .then_with(|| a.pane.cmp(&b.pane))
            });
        }
    }
}

/// Refresh ages (cached rows too), then filter and order.
pub fn display(agents: Vec<Agent>, view: &View, now: f64) -> Vec<Agent> {
    let mut agents = select_agents(agents, view.repo.as_deref(), view.dir.as_deref());
    for agent in &mut agents {
        agent.classify_inactivity(view.dormant_hours, now);
    }
    if view.dormant {
        agents.retain(|a| a.inactivity == Inactivity::Dormant);
    }
    sort_agents(&mut agents, view.sort);
    agents
}

/// Exact repository name (worktree basename or shared repository) and
/// directory-subtree filters; both must pass.
pub fn select_agents(agents: Vec<Agent>, repo: Option<&str>, dir: Option<&Path>) -> Vec<Agent> {
    let mut names: HashMap<String, String> = HashMap::new();
    agents
        .into_iter()
        .filter(|agent| {
            if let Some(repo) = repo
                && agent.repo != repo
            {
                let name = names
                    .entry(agent.cwd.clone())
                    .or_insert_with(|| repository_name(&agent.cwd));
                if name != repo {
                    return false;
                }
            }
            match dir {
                Some(dir) => {
                    !agent.cwd.is_empty() && resolve(Path::new(&agent.cwd)).starts_with(dir)
                }
                None => true,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::model::{Kind, State};
    use std::fs;

    fn agent(pane: &str, cwd: &str, last: Option<f64>) -> Agent {
        let mut a = Agent::new(pane.into(), "s".into(), Kind::Claude);
        a.cwd = cwd.into();
        a.last_input_at = last;
        a
    }

    fn agent_with_state(pane: &str, state: State, last: Option<f64>) -> Agent {
        let mut a = agent(pane, "", last);
        a.state = state;
        a
    }

    fn view() -> View {
        View {
            repo: None,
            dir: None,
            dormant: false,
            dormant_hours: 72.0,
            sort: SortKey::default(),
        }
    }

    fn panes(agents: &[Agent]) -> Vec<&str> {
        agents.iter().map(|a| a.pane.as_str()).collect()
    }

    #[test]
    fn dir_filter_uses_path_components() {
        let t = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(t.path()).unwrap();
        for d in ["project/sub", "project-other"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let s = |p: &str| root.join(p).to_string_lossy().into_owned();
        let agents = vec![
            agent("a", &s("project"), None),
            agent("b", &s("project/sub"), None),
            agent("c", &s("project-other"), None),
            agent("d", "", None),
        ];
        let kept = select_agents(agents, None, Some(&root.join("project")));
        assert_eq!(panes(&kept), ["a", "b"]);
    }

    #[test]
    fn repo_filter_matches_worktree_or_shared_name() {
        let t = tempfile::tempdir().unwrap();
        let main = t.path().join("main");
        fs::create_dir_all(main.join(".git/worktrees/wt")).unwrap();
        fs::write(main.join(".git/worktrees/wt/commondir"), "../..").unwrap();
        fs::create_dir_all(t.path().join("wt")).unwrap();
        fs::write(
            t.path().join("wt/.git"),
            "gitdir: ../main/.git/worktrees/wt",
        )
        .unwrap();
        let mut a = agent("a", &t.path().join("wt").to_string_lossy(), None);
        a.repo = "wt".into();
        let mut b = agent("b", "/nonexistent", None);
        b.repo = "Main".into();
        let agents = vec![a, b];
        assert_eq!(
            panes(&select_agents(agents.clone(), Some("main"), None)),
            ["a"]
        );
        assert_eq!(
            panes(&select_agents(agents.clone(), Some("wt"), None)),
            ["a"]
        );
        assert_eq!(
            panes(&select_agents(agents, Some("list"), None)),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn newest_sort_is_default_descending_by_time_and_unknown_last() {
        let agents = vec![
            agent_with_state("z", State::Idle, None),
            agent_with_state("b", State::Busy, Some(50.0)),
            agent_with_state("a", State::Idle, Some(50.0)),
            agent_with_state("c", State::Input, Some(10.0)),
            agent_with_state("d", State::Idle, Some(90.0)),
            agent_with_state("y", State::Busy, None),
        ];
        // Default (Newest): 90 -> 50 (Busy before Idle) -> 10 -> None (Busy before Idle).
        assert_eq!(
            panes(&display(agents.clone(), &view(), 100.0)),
            ["d", "b", "a", "c", "y", "z"]
        );
        // Oldest: 10 -> 50 (pane a before b) -> 90 -> None (pane y before z).
        let oldest = View {
            sort: SortKey::Oldest,
            ..view()
        };
        assert_eq!(
            panes(&display(agents.clone(), &oldest, 100.0)),
            ["c", "a", "b", "d", "y", "z"]
        );
        // State: Input (c) -> Busy (b 50, y None) -> Idle (d 90, a 50, z None).
        let by_state = View {
            sort: SortKey::State,
            ..view()
        };
        assert_eq!(
            panes(&display(agents, &by_state, 100.0)),
            ["c", "b", "y", "d", "a", "z"]
        );
    }

    #[test]
    fn dormant_view_keeps_only_dormant() {
        let now = 1e6;
        let agents = vec![
            agent("old", "", Some(now - 72.0 * 3600.0 - 2.0)),
            agent("edge", "", Some(now - 72.0 * 3600.0)),
            agent("new", "", Some(now)),
            agent("none", "", None),
        ];
        let v = View {
            dormant: true,
            ..view()
        };
        assert_eq!(panes(&display(agents, &v, now)), ["old"]);
    }
}
