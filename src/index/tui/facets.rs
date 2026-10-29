//! Filter-pane facets (Agent / Project / When / Kind) mapped onto the
//! search [`Filters`], plus the TUI-only project selection.
//!
//! Counts are computed per facet with every *other* facet applied, so a
//! facet's own counts don't collapse when one of its values is unchecked.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Days, NaiveDate};

use crate::index::dates::DateBound;
use crate::index::query::{Filters, Row};
use crate::session::Agent;

/// Projects listed by count before the rest fold into "…N more".
pub const TOP_PROJECTS: usize = 8;

/// The "When" radio: a lower bound on `modified` (plus an upper one for
/// `yesterday`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Today,
    Yesterday,
    ThreeDays,
    Week,
    Month,
    All,
}

impl When {
    pub const ALL: [When; 6] = [
        When::Today,
        When::Yesterday,
        When::ThreeDays,
        When::Week,
        When::Month,
        When::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            When::Today => "today",
            When::Yesterday => "yesterday",
            When::ThreeDays => "3d",
            When::Week => "7d",
            When::Month => "30d",
            When::All => "all",
        }
    }

    /// The `after` bound this choice sets (inclusive day ranges).
    pub fn bound(self, today: NaiveDate) -> Option<DateBound> {
        let back = match self {
            When::Today => 0,
            When::Yesterday => 1,
            When::ThreeDays => 2,
            When::Week => 6,
            When::Month => 29,
            When::All => return None,
        };
        today.checked_sub_days(Days::new(back)).map(DateBound::Day)
    }

    /// The `before` bound this choice sets (inclusive): only `yesterday`.
    pub fn before(self, today: NaiveDate) -> Option<DateBound> {
        match self {
            When::Yesterday => When::Yesterday.bound(today),
            _ => None,
        }
    }

    /// The choice matching `after` / `before`; `None` for a custom date.
    /// Only `yesterday` looks at `before` (a custom `^f` before-date can
    /// combine with the others).
    pub fn of(
        after: Option<DateBound>,
        before: Option<DateBound>,
        today: NaiveDate,
    ) -> Option<When> {
        let y = When::Yesterday;
        if after.is_some() && (after, before) == (y.bound(today), y.before(today)) {
            return Some(y);
        }
        When::ALL
            .into_iter()
            .find(|w| *w != y && w.bound(today) == after)
    }

    /// Select this choice in `f`, dropping the `before` bound `yesterday`
    /// set (but not a custom one).
    pub fn apply(self, f: &mut Filters, today: NaiveDate) {
        if When::of(f.after, f.before, today) == Some(When::Yesterday) {
            f.before = None;
        }
        f.after = self.bound(today);
        if let Some(b) = self.before(today) {
            f.before = Some(b);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindFacet {
    Originals,
    /// Trimmed and rolled-over sessions.
    Derived,
    Sidechains,
}

impl KindFacet {
    pub const ALL: [KindFacet; 3] = [
        KindFacet::Originals,
        KindFacet::Derived,
        KindFacet::Sidechains,
    ];

    pub fn label(self) -> &'static str {
        match self {
            KindFacet::Originals => "originals",
            KindFacet::Derived => "trims / rollovers",
            KindFacet::Sidechains => "sidechains",
        }
    }
}

/// Project facet (TUI-only filter): unchecked projects, or an explicit
/// set to show only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSel {
    pub hidden: BTreeSet<String>,
    /// When non-empty, only these projects are shown.
    pub only: BTreeSet<String>,
}

impl ProjectSel {
    pub fn shows(&self, p: &str) -> bool {
        (self.only.is_empty() || self.only.contains(p)) && !self.hidden.contains(p)
    }

    /// Check / uncheck `p`. With an `only` set the last one stays on.
    pub fn toggle(&mut self, p: &str) {
        if self.only.is_empty() {
            if !self.hidden.remove(p) {
                self.hidden.insert(p.to_string());
            }
        } else if !self.only.insert(p.to_string()) && self.only.len() > 1 {
            self.only.remove(p);
        }
    }

    pub fn only_this(&mut self, p: &str) {
        self.only = [p.to_string()].into();
        self.hidden.clear();
    }

    pub fn clear(&mut self) {
        self.hidden.clear();
        self.only.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.hidden.is_empty() && self.only.is_empty()
    }
}

/// One toggleable row of the filter pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Facet {
    Agent(Agent),
    Project(String),
    /// The "…N more" row: opens the project picker.
    MoreProjects,
    When(When),
    Kind(KindFacet),
}

/// Checkbox state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    On,
    Off,
    /// Only part of a grouped option is on (trims vs rollovers).
    Partial,
}

impl Check {
    fn from(b: bool) -> Check {
        if b { Check::On } else { Check::Off }
    }
}

/// Facet counts for the current query and filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub claude: usize,
    pub codex: usize,
    pub pi: usize,
    pub opencode: usize,
    /// Count desc, then name asc.
    pub projects: Vec<(String, usize)>,
}

impl Counts {
    pub fn agent(&self, a: Agent) -> usize {
        match a {
            Agent::Claude => self.claude,
            Agent::Codex => self.codex,
            Agent::Pi => self.pi,
            Agent::OpenCode => self.opencode,
        }
    }

    fn agent_mut(&mut self, a: Agent) -> &mut usize {
        match a {
            Agent::Claude => &mut self.claude,
            Agent::Codex => &mut self.codex,
            Agent::Pi => &mut self.pi,
            Agent::OpenCode => &mut self.opencode,
        }
    }
}

/// Count `base` (the results with the agent facet relaxed): agents over
/// visible projects, projects over the selected agent(s).
pub fn count(rows: &[Row], base: &[usize], f: &Filters, sel: &ProjectSel) -> Counts {
    let mut c = Counts::default();
    let mut projects: BTreeMap<&str, usize> = BTreeMap::new();
    for r in base.iter().map(|&i| &rows[i]) {
        let project = r.project_name();
        if sel.shows(project) {
            *c.agent_mut(r.agent) += 1;
        }
        if f.agents.contains(r.agent) {
            *projects.entry(project).or_default() += 1;
        }
    }
    c.projects = projects
        .into_iter()
        .map(|(p, n)| (p.to_string(), n))
        .collect();
    c.projects
        .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    c
}

fn project_count(c: &Counts, name: &str) -> usize {
    c.projects
        .iter()
        .find(|(p, _)| p == name)
        .map_or(0, |p| p.1)
}

/// Projects listed in the pane (the `only` ones first, then the top ones,
/// then every hidden one, so each can be unchecked / re-checked) and how
/// many more there are.
pub fn shown_projects(c: &Counts, sel: &ProjectSel) -> (Vec<(String, usize)>, usize) {
    let mut shown: Vec<(String, usize)> = sel
        .only
        .iter()
        .map(|p| (p.clone(), project_count(c, p)))
        .collect();
    for (p, n) in c.projects.iter().take(TOP_PROJECTS) {
        if !sel.only.contains(p) {
            shown.push((p.clone(), *n));
        }
    }
    for h in &sel.hidden {
        if !shown.iter().any(|(p, _)| p == h) {
            shown.push((h.clone(), project_count(c, h)));
        }
    }
    let more = c
        .projects
        .iter()
        .filter(|(p, _)| !shown.iter().any(|(s, _)| s == p))
        .count();
    (shown, more)
}

/// Every project for the picker (counted ones, then selected ones without
/// results) whose name contains `query`, ignoring case.
pub fn picker_projects(c: &Counts, sel: &ProjectSel, query: &str) -> Vec<(String, usize)> {
    let mut all = c.projects.clone();
    for p in sel.only.iter().chain(&sel.hidden) {
        if !all.iter().any(|(q, _)| q == p) {
            all.push((p.clone(), 0));
        }
    }
    let q = query.to_lowercase();
    all.retain(|(p, _)| p.to_lowercase().contains(&q));
    all
}

/// All toggleable rows, top to bottom.
pub fn items(c: &Counts, sel: &ProjectSel) -> Vec<Facet> {
    let mut out: Vec<Facet> = Agent::ALL.map(Facet::Agent).into();
    let (shown, more) = shown_projects(c, sel);
    out.extend(shown.into_iter().map(|(p, _)| Facet::Project(p)));
    if more > 0 {
        out.push(Facet::MoreProjects);
    }
    out.extend(When::ALL.map(Facet::When));
    out.extend(KindFacet::ALL.map(Facet::Kind));
    out
}

/// Whether `facet` is checked.
pub fn state(f: &Filters, sel: &ProjectSel, facet: &Facet, today: NaiveDate) -> Check {
    match facet {
        Facet::Agent(a) => Check::from(f.agents.contains(*a)),
        Facet::Project(p) => Check::from(sel.shows(p)),
        Facet::MoreProjects => Check::Off,
        Facet::When(w) => Check::from(When::of(f.after, f.before, today) == Some(*w)),
        Facet::Kind(KindFacet::Originals) => Check::from(f.include_original),
        Facet::Kind(KindFacet::Sidechains) => Check::from(f.include_sub),
        Facet::Kind(KindFacet::Derived) => match (f.include_trimmed, f.include_continued) {
            (true, true) => Check::On,
            (false, false) => Check::Off,
            _ => Check::Partial,
        },
    }
}

/// Toggle `facet`. At least one agent stays checked (unchecking the last
/// one is a no-op); When is a radio; "…N more" is a no-op (the app opens
/// the picker).
pub fn toggle(f: &mut Filters, sel: &mut ProjectSel, facet: &Facet, today: NaiveDate) {
    match facet {
        Facet::Agent(a) => f.agents.toggle(*a),
        Facet::Project(p) => sel.toggle(p),
        Facet::MoreProjects => {}
        Facet::When(w) => w.apply(f, today),
        Facet::Kind(KindFacet::Originals) => f.include_original = !f.include_original,
        Facet::Kind(KindFacet::Sidechains) => f.include_sub = !f.include_sub,
        Facet::Kind(KindFacet::Derived) => {
            let on = state(f, sel, facet, today) != Check::On;
            f.include_trimmed = on;
            f.include_continued = on;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::index::query::Agents;
    use crate::session::Homes;

    pub(crate) fn row(i: u32, agent: Agent, project: &str) -> Row {
        Row {
            doc: tantivy::DocAddress::new(0, i),
            session_id: format!("{i:08x}-0000-4000-8000-000000000000"),
            agent,
            project: project.into(),
            branch: "main".into(),
            cwd: format!("/w/{project}"),
            created: "2026-10-28T10:00:00Z".into(),
            modified: "2026-10-29T09:58:00Z".into(),
            modified_ts: 0,
            lines: 42,
            file_path: format!("/f/{i}.jsonl"),
            first_msg_role: "user".into(),
            first_msg_content: format!("first message {i}"),
            last_msg_role: "assistant".into(),
            last_msg_content: format!("last message {i}"),
            first_user_msg_content: format!("first message {i}"),
            derivation_type: String::new(),
            is_sidechain: false,
            is_exec_run: false,
            home: String::new(),
            custom_title: String::new(),
        }
    }

    fn filters() -> Filters {
        Filters::new(&Homes::new("/h/c", "/h/x"), "/w".into())
    }

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn counts_relax_their_own_facet() {
        let rows = vec![
            row(0, Agent::Claude, "webapp"),
            row(1, Agent::Claude, "website"),
            row(2, Agent::Codex, "website"),
            row(3, Agent::Codex, "infra"),
        ];
        let base = [0, 1, 2, 3];
        let mut f = filters();
        let mut sel = ProjectSel::default();
        let c = count(&rows, &base, &f, &sel);
        assert_eq!((c.claude, c.codex), (2, 2));
        assert_eq!(
            c.projects,
            [
                ("website".into(), 2),
                ("infra".into(), 1),
                ("webapp".into(), 1)
            ]
        );
        // Codex only: project counts follow, agent counts don't collapse.
        f.agents = Agents::only(Agent::Codex);
        sel.hidden.insert("infra".to_string());
        let c = count(&rows, &base, &f, &sel);
        assert_eq!((c.claude, c.codex), (2, 1));
        assert_eq!(c.projects, [("infra".into(), 1), ("website".into(), 1)]);
        // Only webapp: agent counts follow, project counts don't collapse.
        f.agents = Agents::ALL;
        sel.only_this("webapp");
        let c = count(&rows, &base, &f, &sel);
        assert_eq!((c.claude, c.codex), (1, 0));
        assert_eq!(c.projects.len(), 3);
    }

    #[test]
    fn counts_and_states_cover_all_four_agents() {
        let rows = vec![
            row(0, Agent::Claude, "api"),
            row(1, Agent::Pi, "api"),
            row(2, Agent::Pi, "web"),
            row(3, Agent::OpenCode, "api"),
            row(4, Agent::Codex, "web"),
        ];
        let base = [0, 1, 2, 3, 4];
        let mut f = filters();
        let sel = ProjectSel::default();
        let c = count(&rows, &base, &f, &sel);
        let per_agent = Agent::ALL.map(|a| c.agent(a));
        assert_eq!(per_agent, [1, 1, 2, 1]);
        assert_eq!((c.pi, c.opencode), (2, 1));
        // pi only: project counts follow pi, agent counts stay.
        f.agents = Agents::only(Agent::Pi);
        let c = count(&rows, &base, &f, &sel);
        assert_eq!(Agent::ALL.map(|a| c.agent(a)), [1, 1, 2, 1]);
        assert_eq!(c.projects, [("api".into(), 1), ("web".into(), 1)]);
        let today = day("2026-10-29");
        let checks = Agent::ALL.map(|a| state(&f, &sel, &Facet::Agent(a), today));
        assert_eq!(checks, [Check::Off, Check::Off, Check::On, Check::Off]);
        assert_eq!(&items(&c, &sel)[..4], Agent::ALL.map(Facet::Agent));
    }

    #[test]
    fn hidden_projects_stay_listed() {
        let c = Counts {
            projects: (0..12).map(|i| (format!("p{i:02}"), 20 - i)).collect(),
            ..Counts::default()
        };
        let mut sel = ProjectSel::default();
        sel.hidden.insert("p11".to_string());
        let (shown, more) = shown_projects(&c, &sel);
        assert_eq!(shown.len(), TOP_PROJECTS + 1);
        assert_eq!(shown.last().unwrap(), &("p11".to_string(), 9));
        assert_eq!(more, 3);
        let it = items(&c, &sel);
        assert_eq!(it.len(), 4 + TOP_PROJECTS + 1 + 1 + 6 + 3);
        assert_eq!(it[4 + TOP_PROJECTS + 1], Facet::MoreProjects);
        // `only` projects come first, then the rest of the top ones.
        sel.only_this("p10");
        sel.toggle("p03");
        let (shown, more) = shown_projects(&c, &sel);
        let names: Vec<&str> = shown.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(&names[..3], ["p03", "p10", "p00"]);
        assert_eq!(shown.len(), 2 + TOP_PROJECTS - 1);
        assert_eq!(more, 3);
        // Nothing beyond the top: no "…N more" row.
        let few = Counts {
            projects: c.projects[..3].to_vec(),
            ..Counts::default()
        };
        assert!(!items(&few, &ProjectSel::default()).contains(&Facet::MoreProjects));
    }

    #[test]
    fn when_bounds_round_trip() {
        let today = day("2026-10-29");
        let d = |s: &str| Some(DateBound::Day(day(s)));
        let want = [
            (When::Today, d("2026-10-29"), None),
            (When::Yesterday, d("2026-10-28"), d("2026-10-28")),
            (When::ThreeDays, d("2026-10-27"), None),
            (When::Week, d("2026-10-23"), None),
            (When::Month, d("2026-09-30"), None),
            (When::All, None, None),
        ];
        assert_eq!(want.map(|w| w.0), When::ALL);
        for (w, after, before) in want {
            let mut f = filters();
            w.apply(&mut f, today);
            assert_eq!((f.after, f.before), (after, before), "{w:?}");
            assert_eq!(When::of(f.after, f.before, today), Some(w), "{w:?}");
            assert_eq!(
                state(&f, &ProjectSel::default(), &Facet::When(w), today),
                Check::On
            );
            // Leaving yesterday drops the before bound it set.
            When::Week.apply(&mut f, today);
            assert_eq!((f.after, f.before), (d("2026-10-23"), None), "{w:?} → 7d");
        }
        // A custom before-date survives switching between the others ...
        let mut f = filters();
        f.before = d("2026-10-20");
        When::Month.apply(&mut f, today);
        assert_eq!(When::of(f.after, f.before, today), Some(When::Month));
        When::Today.apply(&mut f, today);
        assert_eq!(f.before, d("2026-10-20"));
        // ... and yesterday's after alone (no before) is a custom date.
        f.after = d("2026-10-28");
        f.before = None;
        assert_eq!(When::of(f.after, f.before, today), None);
    }

    #[test]
    fn project_selection() {
        let mut s = ProjectSel::default();
        assert!(s.is_empty() && s.shows("a") && s.shows("b"));
        s.toggle("a");
        assert!(!s.shows("a") && s.shows("b"));
        s.toggle("a");
        assert!(s.is_empty());
        s.toggle("c");
        s.only_this("a");
        assert!(s.hidden.is_empty(), "only clears hidden");
        assert!(s.shows("a") && !s.shows("b") && !s.shows("c"));
        s.toggle("b");
        assert!(s.shows("a") && s.shows("b"));
        s.toggle("a");
        assert_eq!(s.only, ["b".to_string()].into());
        s.toggle("b");
        assert_eq!(s.only, ["b".to_string()].into(), "last one stays on");
        s.clear();
        assert!(s.is_empty() && s.shows("a"));
    }

    #[test]
    fn picker_lists_all_matching_projects() {
        let c = Counts {
            projects: vec![
                ("Webapp".into(), 5),
                ("infra".into(), 3),
                ("website".into(), 1),
            ],
            ..Counts::default()
        };
        let mut sel = ProjectSel::default();
        sel.hidden.insert("gone".into());
        let names = |q: &str| -> Vec<String> {
            picker_projects(&c, &sel, q)
                .into_iter()
                .map(|(p, _)| p)
                .collect()
        };
        assert_eq!(names(""), ["Webapp", "infra", "website", "gone"]);
        assert_eq!(names("WEB"), ["Webapp", "website"]);
        assert!(names("zz").is_empty());
    }

    #[test]
    fn toggles_map_onto_filters() {
        let today = day("2026-10-29");
        let mut f = filters();
        let mut sel = ProjectSel::default();
        let mut t = |f: &mut Filters, facet: Facet| toggle(f, &mut sel, &facet, today);
        t(&mut f, Facet::Agent(Agent::Claude));
        t(&mut f, Facet::Agent(Agent::Pi));
        t(&mut f, Facet::Agent(Agent::OpenCode));
        assert_eq!(f.agents, Agents::only(Agent::Codex));
        t(&mut f, Facet::Agent(Agent::Codex));
        assert_eq!(f.agents, Agents::only(Agent::Codex), "last agent stays on");
        t(&mut f, Facet::Agent(Agent::OpenCode));
        assert!(!f.agents.contains(Agent::Claude) && f.agents.contains(Agent::OpenCode));
        t(&mut f, Facet::Agent(Agent::Codex));
        assert_eq!(f.agents, Agents::only(Agent::OpenCode));
        t(&mut f, Facet::Agent(Agent::Claude));
        t(&mut f, Facet::Agent(Agent::Codex));
        t(&mut f, Facet::Agent(Agent::Pi));
        assert!(f.agents.is_all());

        t(&mut f, Facet::When(When::Week));
        assert_eq!(f.after, Some(DateBound::Day(day("2026-10-23"))));
        t(&mut f, Facet::When(When::All));
        assert_eq!(f.after, None);
        f.after = Some(DateBound::Day(day("2026-01-01")));
        assert_eq!(When::of(f.after, f.before, today), None, "custom date");

        t(&mut f, Facet::Kind(KindFacet::Sidechains));
        assert!(f.include_sub);
        f.include_continued = false;
        let d = Facet::Kind(KindFacet::Derived);
        assert_eq!(state(&f, &ProjectSel::default(), &d, today), Check::Partial);
        t(&mut f, d.clone());
        assert!(f.include_trimmed && f.include_continued);
        t(&mut f, d);
        assert!(!f.include_trimmed && !f.include_continued);
        t(&mut f, Facet::Kind(KindFacet::Originals));
        assert!(!f.include_original);

        t(&mut f, Facet::Project("webapp".into()));
        t(&mut f, Facet::Project("website".into()));
        t(&mut f, Facet::Project("webapp".into()));
        t(&mut f, Facet::MoreProjects);
        assert_eq!(sel.hidden, ["website".to_string()].into());
    }
}
