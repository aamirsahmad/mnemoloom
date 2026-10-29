//! End-to-end tests over synthetic homes: build, filter, rank, JSON shape.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;
use crate::session::testutil::*;

struct Env {
    _root: tempfile::TempDir,
    homes: Homes,
    idx: PathBuf,
}

fn env() -> Env {
    let root = tempfile::tempdir().unwrap();
    let homes = Homes::new(root.path().join("claude"), root.path().join("codex")).canonical();
    let idx = root.path().join("idx");
    Env {
        _root: root,
        homes,
        idx,
    }
}

const ID3: &str = "c1a0de00-3333-4abc-8def-012345678913";
const ID4: &str = "c1a0de00-4444-4abc-8def-012345678914";

/// Claude transcript with `prompt` as the real user message and the last
/// timestamp at `ts`.
fn claude_with(id: &str, cwd: &str, prompt: &str, ts: &str) -> Vec<String> {
    claude_lines(id, cwd)
        .into_iter()
        .map(|l| {
            l.replace("fix the failing spec in user_spec.rb", prompt)
                .replace("2026-01-03T03:30:00.000Z", ts)
        })
        .collect()
}

fn catalog(e: &Env) -> Catalog {
    build::update(&e.idx, &e.homes, None, store::LockWait::Block).unwrap();
    Catalog::open(&e.idx, &e.homes).unwrap().unwrap()
}

fn ids(cat: &Catalog, sel: &[usize]) -> Vec<String> {
    sel.iter()
        .map(|&i| cat.rows[i].session_id.clone())
        .collect()
}

fn filters(e: &Env, cwd: &str) -> Filters {
    Filters::new(&e.homes, cwd.to_string())
}

#[test]
fn rows_round_trip_from_fast_fields() {
    let e = env();
    write_claude(
        &e.homes.claude,
        CLAUDE_ID,
        "/w/a",
        &claude_lines(CLAUDE_ID, "/w/a"),
    );
    write_codex(
        &e.homes.codex,
        CODEX_ID,
        "2026-01-02T12-57-30",
        &codex_lines(CODEX_ID, "/w/c", "cli"),
    );
    let cat = catalog(&e);
    assert_eq!(cat.rows.len(), 2);
    let c = &cat.rows[0];
    assert_eq!(c.session_id, CLAUDE_ID);
    assert_eq!(
        (c.agent, c.project.as_str(), c.branch.as_str()),
        (Agent::Claude, "a", "main")
    );
    assert_eq!(c.lines, 1);
    assert_eq!(c.custom_title, "renamed-later");
    assert_eq!(c.home, e.homes.claude.to_string_lossy());
    assert_eq!(c.first_msg(), "fix the failing spec in user_spec.rb");
    let x = &cat.rows[1];
    assert_eq!(
        (x.agent, x.is_exec_run, x.is_sidechain),
        (Agent::Codex, false, false)
    );
    assert_eq!(x.home, e.homes.codex.to_string_lossy());
}

#[test]
fn scope_type_agent_and_line_filters() {
    let e = env();
    write_claude(
        &e.homes.claude,
        CLAUDE_ID,
        "/w/a",
        &claude_lines(CLAUDE_ID, "/w/a"),
    );
    write_claude(
        &e.homes.claude,
        CLAUDE_ID2,
        "/w/a/sub",
        &claude_lines(CLAUDE_ID2, "/w/a/sub"),
    );
    write_codex(
        &e.homes.codex,
        CODEX_ID,
        "2026-01-02T12-57-30",
        &codex_lines(CODEX_ID, "/w/a", "exec"),
    );
    let mut trimmed = claude_lines(ID3, "/w/a");
    trimmed[0] = trimmed[0].replace(
        r#"{"type":"permission-mode""#,
        r#"{"trim_metadata":{"parent_file":"/x.jsonl"},"type":"permission-mode""#,
    );
    write_claude(&e.homes.claude, ID3, "/w/a", &trimmed);
    let cat = catalog(&e);
    let run = |f: &Filters| {
        let mut v = ids(&cat, &query::select(&cat, f, None, false, None, None));
        v.sort();
        v
    };
    let sorted = |v: &[&str]| {
        let mut v: Vec<String> = v.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    };
    let mut f = filters(&e, "/w/a");
    // Exact cwd by default; exec runs hidden.
    assert_eq!(run(&f), sorted(&[CLAUDE_ID, ID3]));
    f.include_exec = true;
    assert_eq!(run(&f), sorted(&[CODEX_ID, CLAUDE_ID, ID3]));
    f.agents = Agents::only(Agent::Codex);
    assert_eq!(run(&f), vec![CODEX_ID]);
    f.agents = Agents::ALL;
    f.include_exec = false;
    f.include_trimmed = false;
    assert_eq!(run(&f), vec![CLAUDE_ID]);
    // --dir is a prefix match.
    let mut f = filters(&e, "/elsewhere");
    f.dir = Some("/w/a".into());
    assert_eq!(run(&f), sorted(&[CLAUDE_ID2, CLAUDE_ID, ID3]));
    f.dir = Some("/w".into());
    f.branch = Some("nope".into());
    assert!(run(&f).is_empty());
    // Branch ignored when global.
    let mut f = filters(&e, "/elsewhere");
    f.global = true;
    f.branch = Some("nope".into());
    assert_eq!(run(&f).len(), 3);
    f.min_lines = Some(2);
    assert!(run(&f).is_empty());
    // Other homes' documents are hidden.
    let mut f = filters(&e, "/w/a");
    f.claude_home = "/other".into();
    assert!(run(&f).is_empty());
}

#[test]
fn date_filters() {
    let e = env();
    write_claude(
        &e.homes.claude,
        CLAUDE_ID,
        "/w",
        &claude_with(CLAUDE_ID, "/w", "p", "2026-10-01T12:00:00Z"),
    );
    write_claude(
        &e.homes.claude,
        CLAUDE_ID2,
        "/w",
        &claude_with(CLAUDE_ID2, "/w", "p", "2026-10-05T12:00:00Z"),
    );
    let cat = catalog(&e);
    let mut f = filters(&e, "/w");
    f.after = dates::parse_date_now("2026-10-02");
    assert_eq!(
        ids(&cat, &query::select(&cat, &f, None, false, None, None)),
        vec![CLAUDE_ID2]
    );
    f.after = None;
    f.before = dates::parse_date_now("2026-10-01");
    assert_eq!(
        ids(&cat, &query::select(&cat, &f, None, false, None, None)),
        vec![CLAUDE_ID]
    );
}

#[test]
fn ranking_phrase_and_recency() {
    let e = env();
    let now = SystemTime::now();
    let iso = |ago_days: u64| {
        let t = now - Duration::from_secs(ago_days * 86_400);
        chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()
    };
    // Same text, different age: newer ranks first.
    write_claude(
        &e.homes.claude,
        CLAUDE_ID,
        "/w",
        &claude_with(CLAUDE_ID, "/w", "rocket launch window", &iso(60)),
    );
    write_claude(
        &e.homes.claude,
        CLAUDE_ID2,
        "/w",
        &claude_with(CLAUDE_ID2, "/w", "rocket launch window", &iso(1)),
    );
    // Both words but not as a phrase: below the phrase matches.
    write_claude(
        &e.homes.claude,
        ID3,
        "/w",
        &claude_with(ID3, "/w", "launch the big rocket", &iso(0)),
    );
    write_claude(
        &e.homes.claude,
        ID4,
        "/w",
        &claude_with(ID4, "/w", "unrelated words", &iso(0)),
    );
    let cat = catalog(&e);
    let f = filters(&e, "/w");
    let q = query::run_query(&cat, "rocket launch", &f, now);
    let sel = query::select(&cat, &f, Some(&q), false, None, None);
    assert_eq!(ids(&cat, &sel), vec![CLAUDE_ID2, CLAUDE_ID, ID3]);
    // --by-time keeps the hit set but orders by modified.
    let sel = query::select(&cat, &f, Some(&q), true, None, None);
    assert_eq!(ids(&cat, &sel), vec![ID3, CLAUDE_ID2, CLAUDE_ID]);
    // -n caps.
    assert_eq!(
        query::select(&cat, &f, Some(&q), false, Some(1), None).len(),
        1
    );
    // Field syntax works.
    let q = query::run_query(&cat, "agent:codex", &f, now);
    assert!(query::select(&cat, &f, Some(&q), false, None, None).is_empty());
    assert!((query::recency(0.0) - 2.0).abs() < 1e-9);
    assert!(query::recency(1e12) < 1.0001);
}

#[test]
fn query_on_chosen_docs_ands_units_and_excludes() {
    let e = env();
    let ts = "2026-10-01T00:00:00Z";
    for (id, text) in [
        (CLAUDE_ID, "rocket launch window"),
        (CLAUDE_ID2, "rocket fuel"),
        (ID3, "launch rocket fuel"),
    ] {
        write_claude(&e.homes.claude, id, "/w", &claude_with(id, "/w", text, ts));
    }
    let cat = catalog(&e);
    let doc = |id: &str| cat.rows.iter().find(|r| r.session_id == id).unwrap().doc;
    let ids_of = |q: &query::TextQuery| -> Vec<String> {
        let mut v: Vec<String> = q
            .ranked
            .iter()
            .map(|d| {
                cat.rows
                    .iter()
                    .find(|r| r.doc == *d)
                    .unwrap()
                    .session_id
                    .clone()
            })
            .collect();
        v.sort();
        v
    };
    let all = [doc(CLAUDE_ID), doc(CLAUDE_ID2), doc(ID3)];
    let q = query::run_query_docs(&cat, "rocket launch", "", &all, UNIX_EPOCH);
    assert_eq!(ids_of(&q), vec![CLAUDE_ID, ID3]);
    assert_eq!(q.scores.len(), 2);
    // The phrase ranks first.
    assert_eq!(q.ranked[0], doc(CLAUDE_ID));
    assert!(q.scores[0] > q.scores[1]);
    // Only the chosen documents are scored.
    let q = query::run_query_docs(&cat, "rocket", "", &[doc(ID3)], UNIX_EPOCH);
    assert_eq!(ids_of(&q), vec![ID3]);
    let q = query::run_query_docs(&cat, "rocket", "fuel \"no such\"", &all, UNIX_EPOCH);
    assert_eq!(ids_of(&q), vec![CLAUDE_ID]);
    let q = query::run_query_docs(&cat, "\"window rocket\"", "", &all, UNIX_EPOCH);
    assert!(q.ranked.is_empty());
    let s = q.snippet(&cat, &cat.searcher(), doc(CLAUDE_ID));
    assert!(s.text.contains("rocket launch window"), "{}", s.text);
}

#[test]
fn snippets_are_plain_text() {
    let e = env();
    write_claude(
        &e.homes.claude,
        CLAUDE_ID,
        "/w",
        &claude_with(
            CLAUDE_ID,
            "/w",
            "compare <a> & \\\"b\\\" for zebra",
            "2026-10-01T00:00:00Z",
        ),
    );
    let cat = catalog(&e);
    let f = filters(&e, "/w");
    let q = query::run_query(&cat, "zebra", &f, UNIX_EPOCH);
    let sel = query::select(&cat, &f, Some(&q), false, None, None);
    let s = q.snippet(&cat, &cat.searcher(), cat.rows[sel[0]].doc);
    assert!(
        s.text.contains("compare <a> & \"b\" for zebra"),
        "{}",
        s.text
    );
    assert_eq!(&s.text[s.highlights[0].clone()], "zebra");
}

#[test]
fn json_shape() {
    let e = env();
    write_claude(
        &e.homes.claude,
        CLAUDE_ID,
        "/w/a",
        &claude_lines(CLAUDE_ID, "/w/a"),
    );
    let cat = catalog(&e);
    let line = json_line(&cat.rows[0], None);
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    assert_eq!(
        keys,
        [
            "agent",
            "branch",
            "created",
            "custom_title",
            "cwd",
            "derivation_type",
            "file_path",
            "first_msg",
            "is_exec_run",
            "is_sidechain",
            "last_msg",
            "lines",
            "modified",
            "project",
            "session_id",
            "snippet"
        ]
    );
    assert!(line.starts_with(r#"{"agent":"claude","branch":"main","#));
    assert_eq!(v["snippet"], serde_json::Value::Null);
    assert_eq!(v["lines"], 1);
    assert_eq!(v["is_sidechain"], false);
    let s = Snippet {
        text: "a <b>".into(),
        highlights: vec![],
    };
    let v: serde_json::Value = serde_json::from_str(&json_line(&cat.rows[0], Some(&s))).unwrap();
    assert_eq!(v["snippet"], "a <b>");
}

#[test]
fn dir_branch_spec() {
    assert_eq!(split_dir_branch("/a/b:main"), ("/a/b", Some("main")));
    assert_eq!(split_dir_branch("/a/b:feat/x"), ("/a/b", Some("feat/x")));
    let dir = tempfile::tempdir().unwrap();
    let odd = dir.path().join("a:b");
    std::fs::create_dir(&odd).unwrap();
    let odd = odd.to_string_lossy();
    assert_eq!(split_dir_branch(&odd), (&*odd, None));
    assert_eq!(split_dir_branch("/a/b"), ("/a/b", None));
    assert_eq!(split_dir_branch(":dev"), ("", Some("dev")));
    assert_eq!(
        resolve_dir("x/y", "/nonexistent-base"),
        "/nonexistent-base/x/y"
    );
    assert_eq!(resolve_dir("/", "/q"), "/");
    assert!(Path::new(&resolve_dir("~", "/")).is_absolute());
}

#[test]
fn stats_report_reconciles() {
    let e = env();
    write_claude(
        &e.homes.claude,
        CLAUDE_ID,
        "/w/a",
        &claude_lines(CLAUDE_ID, "/w/a"),
    );
    let empty: Vec<String> = claude_lines(CLAUDE_ID2, "/w/b")
        .into_iter()
        .take(3)
        .collect();
    write_claude(&e.homes.claude, CLAUDE_ID2, "/w/b", &empty);
    let cat = catalog(&e);
    let disk = scan_disk(&e.homes);
    let state = store::load_state(&e.idx);
    let report = stats_report(&disk, &cat.rows, &state, &e.homes, Some("/w/a"));
    assert!(report.contains("Claude resumable: 2"), "{report}");
    assert!(report.contains("Not indexable:  1"), "{report}");
    assert!(report.contains("✅ Index is fully in sync"), "{report}");
    let report = stats_report(
        &disk,
        &cat.rows,
        &store::State::new(),
        &e.homes,
        Some("/w/a"),
    );
    assert!(report.contains("❌ Missing from index: 1"), "{report}");
    assert!(report.contains(&format!("   - {CLAUDE_ID2}.jsonl (claude, resumable)")));
    assert!(report.contains("With cwd '/w/a':\n  Documents: 1"));
    assert!(report.contains("     1 | /w/a"));
}

/// Sessions whose prompt contains query-syntax characters.
#[test]
fn query_syntax_characters_are_plain_text() {
    let e = env();
    let prompts = [
        (
            "c1a0de00-a001-4abc-8def-012345678901",
            "use std::fs to read",
        ),
        (
            "c1a0de00-a002-4abc-8def-012345678902",
            "server on localhost:8080 is down",
        ),
        (
            "c1a0de00-a003-4abc-8def-012345678903",
            "TODO: wire the cache",
        ),
        ("c1a0de00-a004-4abc-8def-012345678904", "alpha beta gamma"),
        ("c1a0de00-a005-4abc-8def-012345678905", "call f(x) twice"),
        (
            "c1a0de00-a006-4abc-8def-012345678906",
            "port it to C++ quickly",
        ),
    ];
    for (id, p) in prompts {
        write_claude(
            &e.homes.claude,
            id,
            "/w",
            &claude_with(id, "/w", p, "2026-10-01T00:00:00Z"),
        );
    }
    let cat = catalog(&e);
    let f = filters(&e, "/w");
    let top = |q: &str| {
        let tq = query::run_query(&cat, q, &f, UNIX_EPOCH);
        ids(&cat, &query::select(&cat, &f, Some(&tq), false, None, None))
    };
    for (q, want) in [
        ("std::fs", 0),
        ("localhost:8080", 1),
        ("TODO:", 2),
        ("\"alpha beta\"", 3),
        ("(x)", 4),
        ("C++", 5),
        ("f(x)", 4),
        ("+cache -nothing", 2),
    ] {
        let got = top(q);
        assert_eq!(
            got.first().map(String::as_str),
            Some(prompts[want].0),
            "{q}: {got:?}"
        );
    }
    // Quoted phrases must be adjacent; unquoted words are OR-ed.
    assert!(top("\"beta alpha\"").is_empty());
    assert_eq!(top("beta alpha").len(), 1);
    assert!(top("::").is_empty());
}

#[test]
fn home_spellings_share_one_set_of_keys() {
    let e = env();
    write_claude(
        &e.homes.claude,
        CLAUDE_ID,
        "/w",
        &claude_lines(CLAUDE_ID, "/w"),
    );
    let link = e.idx.with_file_name("link");
    std::os::unix::fs::symlink(&e.homes.claude, &link).unwrap();
    let parent = e.homes.claude.parent().unwrap().display().to_string();
    let spellings = [
        e.homes.claude.display().to_string(),
        format!("{}/", e.homes.claude.display()),
        format!("{parent}/./claude"),
        format!("{parent}/claude/../claude"),
        link.display().to_string(),
    ];
    for sp in &spellings {
        let h = Homes::new(sp, &e.homes.codex);
        let s = build::update(&e.idx, &h, None, store::LockWait::Block).unwrap();
        assert!(s.indexed <= 1, "{sp}: re-indexed under a new key");
        let cat = Catalog::open(&e.idx, &h).unwrap().unwrap();
        assert_eq!(cat.rows.len(), 1, "{sp}");
        let f = Filters::new(&h, "/w".into());
        assert_eq!(
            query::select(&cat, &f, None, false, None, None).len(),
            1,
            "{sp}"
        );
    }
    assert_eq!(store::load_state(&e.idx).len(), 1);
}

#[test]
fn non_canonical_state_keys_are_pruned() {
    let e = env();
    let p = write_claude(
        &e.homes.claude,
        CLAUDE_ID,
        "/w",
        &claude_lines(CLAUDE_ID, "/w"),
    );
    build::update(&e.idx, &e.homes, None, store::LockWait::Block).unwrap();
    // A key recorded under another spelling (as an older index could have).
    let mut st = store::load_state(&e.idx);
    let odd = format!("{}/./projects", e.homes.claude.display());
    let odd_key = p
        .to_string_lossy()
        .replace(&format!("{}/projects", e.homes.claude.display()), &odd);
    let entry = st.values().next().unwrap().clone();
    st.insert(odd_key.clone(), entry);
    store::save_state(&e.idx, &st).unwrap();
    let s = build::update(&e.idx, &e.homes, None, store::LockWait::Block).unwrap();
    assert_eq!(s.pruned, 1);
    assert!(!store::load_state(&e.idx).contains_key(&odd_key));
}

#[test]
fn concurrent_updates_on_a_fresh_dir() {
    let e = env();
    for (i, id) in [CLAUDE_ID, CLAUDE_ID2, ID3, ID4].iter().enumerate() {
        write_claude(
            &e.homes.claude,
            id,
            &format!("/w/{i}"),
            &claude_lines(id, &format!("/w/{i}")),
        );
    }
    for round in 0..10 {
        let dir = e.idx.with_file_name(format!("race{round}"));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let (dir, homes) = (dir.clone(), e.homes.clone());
                std::thread::spawn(move || {
                    build::update(&dir, &homes, None, store::LockWait::Block)
                })
            })
            .collect();
        let indexed: usize = handles
            .into_iter()
            .map(|h| h.join().unwrap().unwrap().indexed)
            .sum();
        assert_eq!(indexed, 4, "round {round}");
        let cat = Catalog::open(&dir, &e.homes).unwrap().unwrap();
        assert_eq!(cat.rows.len(), 4, "round {round}");
    }
}

/// [`env`] with pi and opencode homes holding [`PI_ID`] (cwd `/w/a`) and
/// the opencode parent + child (cwd `/w/a`), next to one Claude session.
fn env_all_agents() -> Env {
    let root = tempfile::tempdir().unwrap();
    let homes = Homes::new(root.path().join("claude"), root.path().join("codex"))
        .with_pi(root.path().join("pi"))
        .with_opencode(root.path().join("opencode"))
        .canonical();
    write_claude(
        &homes.claude,
        CLAUDE_ID,
        "/w/a",
        &claude_lines(CLAUDE_ID, "/w/a"),
    );
    write_pi(&homes.pi, PI_ID, "/w/a", &pi_lines(PI_ID, "/w/a"));
    write_opencode_db(
        &homes.opencode.join("opencode.db"),
        "/w/a",
        1_792_644_400_000,
    );
    let idx = root.path().join("idx");
    Env {
        _root: root,
        homes,
        idx,
    }
}

#[test]
fn pi_and_opencode_are_indexed_filtered_and_searched() {
    let e = env_all_agents();
    let cat = catalog(&e);
    assert_eq!(cat.rows.len(), 4);
    let by_agent = |a: Agent| cat.rows.iter().filter(|r| r.agent == a).count();
    assert_eq!(Agent::ALL.map(by_agent), [1, 0, 1, 2]);
    let pi = cat.rows.iter().find(|r| r.agent == Agent::Pi).unwrap();
    assert_eq!((pi.session_id.as_str(), pi.lines), (PI_ID, 2));
    assert_eq!(pi.home, e.homes.pi.to_string_lossy());
    let child = cat
        .rows
        .iter()
        .find(|r| r.session_id == OPENCODE_CHILD_ID)
        .unwrap();
    assert!(child.is_sidechain);
    assert_eq!(child.home, e.homes.opencode.to_string_lossy());

    let run = |f: &Filters, q: Option<&str>| {
        let tq = q.map(|q| query::run_query(&cat, q, f, SystemTime::now()));
        let mut v = ids(
            &cat,
            &query::select(&cat, f, tq.as_ref(), false, None, None),
        );
        v.sort();
        v
    };
    let mut f = filters(&e, "/w/a");
    // Sidechains (the opencode child) hidden by default.
    let mut all = vec![CLAUDE_ID.to_string(), PI_ID.into(), OPENCODE_ID.into()];
    all.sort();
    assert_eq!(run(&f, None), all);
    f.agents = Agents::only(Agent::Pi);
    assert_eq!(run(&f, None), [PI_ID]);
    f.agents = Agents::only(Agent::OpenCode);
    assert_eq!(run(&f, None), [OPENCODE_ID]);
    f.include_sub = true;
    let mut oc = vec![OPENCODE_ID.to_string(), OPENCODE_CHILD_ID.into()];
    oc.sort();
    assert_eq!(run(&f, None), oc);
    // Full text: an abandoned pi branch is searchable; tool output too.
    f.agents = Agents::ALL;
    assert_eq!(run(&f, Some("redis")), [PI_ID]);
    assert_eq!(run(&f, Some("explore")), [OPENCODE_CHILD_ID]);
    // Another (or no) pi / opencode home hides those rows.
    let mut g = filters(&e, "/w/a");
    g.pi_home = "/other".into();
    g.opencode_home = String::new();
    assert_eq!(run(&g, None), [CLAUDE_ID]);
    assert!(run(&g, Some("redis")).is_empty());
}

#[test]
fn search_agent_flag_maps_to_agent_sets() {
    let homes = Homes::new("/h/c", "/h/x");
    for (agent, want) in [
        (SearchAgent::All, Agents::ALL),
        (SearchAgent::Claude, Agents::only(Agent::Claude)),
        (SearchAgent::Codex, Agents::only(Agent::Codex)),
        (SearchAgent::Pi, Agents::only(Agent::Pi)),
        (SearchAgent::OpenCode, Agents::only(Agent::OpenCode)),
    ] {
        let args = SearchArgs {
            agent,
            ..Default::default()
        };
        let f = filters_from_args(&args, &homes, "/w".into()).unwrap();
        assert_eq!(f.agents, want, "{agent:?}");
    }
}

#[test]
fn json_lines_for_pi_and_opencode() {
    let e = env_all_agents();
    let cat = catalog(&e);
    let json = |id: &str| -> serde_json::Value {
        let r = cat.rows.iter().find(|r| r.session_id == id).unwrap();
        serde_json::from_str(&json_line(r, None)).unwrap()
    };
    let pi = json(PI_ID);
    assert_eq!(pi["agent"], "pi");
    assert_eq!(pi["custom_title"], "rate limiter");
    assert_eq!(pi["first_msg"], "add a rate limiter");
    assert_eq!(pi["last_msg"], "Token bucket added.");
    assert_eq!(pi["lines"], 2);
    assert!(pi["file_path"].as_str().unwrap().ends_with(".jsonl"));
    let oc = json(OPENCODE_ID);
    assert_eq!(oc["agent"], "opencode");
    assert_eq!(oc["custom_title"], "Rate limiter");
    assert_eq!(oc["is_sidechain"], false);
    assert!(
        oc["file_path"]
            .as_str()
            .unwrap()
            .ends_with(&format!("opencode.db#{OPENCODE_ID}"))
    );
    let child = json(OPENCODE_CHILD_ID);
    assert_eq!(child["agent"], "opencode");
    assert_eq!(child["is_sidechain"], true);
}
