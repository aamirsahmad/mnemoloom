//! Display-time views: repository/directory selection, dormancy, sort order.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::git::repository_name;
use super::model::{Agent, Inactivity};
use super::paths::resolve;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum SortKey {
    /// Current state (urgency), as scanned.
    #[default]
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

/// Refresh ages (cached rows too), then filter and order.
pub fn display(agents: Vec<Agent>, view: &View, now: f64) -> Vec<Agent> {
    let mut agents = select_agents(agents, view.repo.as_deref(), view.dir.as_deref());
    for agent in &mut agents {
        agent.classify_inactivity(view.dormant_hours, now);
    }
    if view.dormant {
        agents.retain(|a| a.inactivity == Inactivity::Dormant);
    }
    if view.sort == SortKey::Oldest {
        agents.sort_by(|a, b| {
            let key = |x: &Agent| (x.last_input_at.is_none(), x.last_input_at.unwrap_or(0.0));
            let (ka, kb) = (key(a), key(b));
            ka.0.cmp(&kb.0)
                .then(ka.1.total_cmp(&kb.1))
                .then_with(|| a.pane.cmp(&b.pane))
        });
    }
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
    use crate::mux::model::Kind;
    use std::fs;

    fn agent(pane: &str, cwd: &str, last: Option<f64>) -> Agent {
        let mut a = Agent::new(pane.into(), "s".into(), Kind::Claude);
        a.cwd = cwd.into();
        a.last_input_at = last;
        a
    }

    fn view() -> View {
        View {
            repo: None,
            dir: None,
            dormant: false,
            dormant_hours: 72.0,
            sort: SortKey::State,
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
    fn oldest_sort_puts_unknown_last() {
        let agents = vec![
            agent("z", "", None),
            agent("b", "", Some(50.0)),
            agent("a", "", Some(50.0)),
            agent("c", "", Some(10.0)),
            agent("y", "", None),
        ];
        let v = View {
            sort: SortKey::Oldest,
            ..view()
        };
        assert_eq!(
            panes(&display(agents.clone(), &v, 100.0)),
            ["c", "a", "b", "y", "z"]
        );
        assert_eq!(
            panes(&display(agents, &view(), 100.0)),
            ["z", "b", "a", "c", "y"]
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
