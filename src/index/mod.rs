//! Search index (tantivy) and the `search` command.
//!
//! - [`schema`]: the 21 index fields.
//! - [`extract`]: per-file document extraction via `crate::session`.
//! - [`store`]: index location (`~/.cache/mnemoloom/search-index`,
//!   `$MNEMOLOOM_INDEX_DIR`), version stamp, incremental state.
//! - [`build`]: incremental (re)indexing, rayon-parallel.
//! - [`query`]: rows from fast fields, filters, ranking.
//! - [`snippet`], [`dates`], [`live`]: helpers.
//! - [`tui`]: the interactive search UI.

pub mod build;
pub mod dates;
pub mod extract;
pub mod live;
pub mod query;
pub mod schema;
pub mod snippet;
pub mod store;
pub mod tui;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use serde_json::{Value, json};

use crate::cli::{
    BuildIndexArgs, ClearIndexArgs, GlobalArgs, IndexStatsArgs, SearchAgent, SearchArgs,
};
use crate::exit::Exit;
use crate::session::discover::{
    claude_session_paths, codex_session_paths, opencode_sessions, scan_paths,
};
use crate::session::homes::expand_tilde;
use crate::session::{Agent, Depth, Homes, absolute, opencode, pi};
use build::UpdateStats;
use dates::parse_date_now;
use query::{Agents, Catalog, Filters, Row};
use snippet::Snippet;

/// `mnemoloom build-index`: incremental index update over every home.
pub fn build_index(global: &GlobalArgs, args: &BuildIndexArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?.canonical();
    let dir = store::index_dir_arg(args.index.as_deref());
    println!("Building search index...");
    println!("Index: {}", dir.display());
    let claude_n = claude_session_paths(&homes.claude, true).len();
    let codex_n = codex_session_paths(&homes.codex).len();
    let pi_n = pi::session_paths(&homes.pi).len();
    let opencode_n = opencode::all_sessions(&homes.opencode).0.len();
    println!("Claude home: {} ({claude_n} files)", homes.claude.display());
    println!("Codex home:  {} ({codex_n} files)", homes.codex.display());
    if homes.has(Agent::Pi) {
        println!("pi home:     {} ({pi_n} files)", homes.pi.display());
    }
    if homes.has(Agent::OpenCode) {
        println!(
            "opencode home: {} ({opencode_n} sessions)",
            homes.opencode.display()
        );
    }
    println!(
        "Total: {} session files to check",
        claude_n + codex_n + pi_n + opencode_n
    );
    let started = std::time::Instant::now();
    let progress = |done: usize, total: usize| {
        if done == total || done.is_multiple_of(25) {
            eprint!("\rIndexing sessions: {done}/{total} files");
            if done == total {
                eprintln!();
            }
        }
    };
    if store::lock(&dir, store::LockWait::Try)?.is_none() {
        println!("Waiting for another mnemoloom process to finish indexing...");
    }
    let stats = build::update(&dir, &homes, Some(&progress), store::LockWait::Block)?;
    report_update(&stats, true);
    print_build_summary(&stats);
    println!("   Time:    {:.2}s", started.elapsed().as_secs_f64());
    Ok(ExitCode::SUCCESS)
}

fn report_update(stats: &UpdateStats, verbose: bool) {
    for n in &stats.notes {
        eprintln!("{n}");
    }
    if stats.indexed > 0 {
        println!("Indexed {} new/modified sessions", stats.indexed);
    }
    if stats.pruned > 0 {
        println!("Pruned {} deleted sessions from index", stats.pruned);
    }
    if verbose && !stats.changed() {
        println!("Index up to date");
    }
}

fn print_build_summary(s: &UpdateStats) {
    println!("\n✅ Index build complete:");
    println!("   Indexed: {}", s.indexed);
    println!("   Skipped: {} (unchanged)", s.skipped);
    if s.failed > 0 {
        println!(
            "   Failed:  {} (empty: {}, parse: {}, index: {})",
            s.failed, s.empty, s.parse_error, s.index_error
        );
    } else {
        println!("   Failed:  0");
    }
    println!(
        "   Total:   {} (Claude: {}, Codex: {}, pi: {}, opencode: {})",
        s.total_files, s.claude_files, s.codex_files, s.pi_files, s.opencode_sessions
    );
}

/// `mnemoloom clear-index [-i PATH] [-n]`.
pub fn clear_index(_global: &GlobalArgs, args: &ClearIndexArgs) -> anyhow::Result<ExitCode> {
    let dir = store::index_dir_arg(args.index.as_deref());
    if !dir.exists() {
        println!("Index directory does not exist: {}", dir.display());
        return Ok(ExitCode::SUCCESS);
    }
    if !store::is_ours_or_empty(&dir) {
        return Err(Exit::fail(format!(
            "Error: Refusing to clear {}: it is not an mnemoloom search index.",
            dir.display()
        ))
        .into());
    }
    let n = count_files(&dir);
    if args.dry_run {
        println!("Dry run: would delete index at {}", dir.display());
        println!("   {n} files would be removed");
    } else {
        store::clear(&dir)?;
        println!("✅ Cleared index at {}", dir.display());
        println!("   {n} files removed");
    }
    Ok(ExitCode::SUCCESS)
}

fn count_files(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| match e.file_type() {
                    Ok(t) if t.is_dir() => count_files(&e.path()),
                    Ok(_) => 1,
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

/// `mnemoloom index-stats [-i PATH] [-c CWD]`.
pub fn index_stats(global: &GlobalArgs, args: &IndexStatsArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?.canonical();
    let dir = store::index_dir_arg(args.index.as_deref());
    let Some(cat) = Catalog::open(&dir, &homes)? else {
        println!("Index not found at {}", dir.display());
        return Ok(ExitCode::SUCCESS);
    };
    println!("Scanning filesystem...");
    let disk = scan_disk(&homes);
    print!(
        "{}",
        stats_report(
            &disk,
            &cat.rows,
            &store::load_state(&dir),
            &homes,
            args.cwd.as_deref()
        )
    );
    Ok(ExitCode::SUCCESS)
}

/// A session file on disk as `index-stats` sees it.
struct DiskFile {
    path: String,
    agent: Agent,
    subagent: bool,
}

struct DiskScan {
    files: Vec<DiskFile>,
    invalid: usize,
}

fn scan_disk(homes: &Homes) -> DiskScan {
    let paths: Vec<(Agent, PathBuf)> = claude_session_paths(&homes.claude, true)
        .into_iter()
        .map(|p| (Agent::Claude, p))
        .chain(
            codex_session_paths(&homes.codex)
                .into_iter()
                .map(|p| (Agent::Codex, p)),
        )
        .chain(
            pi::session_paths(&homes.pi)
                .into_iter()
                .map(|p| (Agent::Pi, p)),
        )
        .collect();
    let mut scan = DiskScan {
        files: Vec::new(),
        invalid: 0,
    };
    let mut sessions = scan_paths(paths, Depth::Header);
    sessions.extend(opencode_sessions(&homes.opencode, Depth::Header));
    for sf in sessions {
        let in_home = sf.cwd.as_deref().is_some_and(|c| {
            Agent::ALL
                .into_iter()
                .filter(|a| homes.has(*a))
                .any(|a| Path::new(c).starts_with(absolute(homes.home(a))))
        });
        if in_home || sf.is_helper {
            continue;
        }
        if !sf.is_valid {
            scan.invalid += 1;
            continue;
        }
        scan.files.push(DiskFile {
            path: sf.path.to_string_lossy().into_owned(),
            agent: sf.agent,
            subagent: sf.is_sidechain,
        });
    }
    scan
}

fn kind(subagent: bool) -> &'static str {
    if subagent { "subagent" } else { "resumable" }
}

fn file_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn stats_report(
    disk: &DiskScan,
    rows: &[Row],
    state: &store::State,
    homes: &Homes,
    cwd: Option<&str>,
) -> String {
    let mut o = String::new();
    let count = |a: Agent, sub: bool| {
        disk.files
            .iter()
            .filter(|f| f.agent == a && f.subagent == sub)
            .count()
    };
    let w = |o: &mut String, s: String| {
        o.push_str(&s);
        o.push('\n');
    };
    w(&mut o, "\n=== Filesystem ===".into());
    w(
        &mut o,
        format!("Claude resumable: {}", count(Agent::Claude, false)),
    );
    w(
        &mut o,
        format!("Claude subagent:  {}", count(Agent::Claude, true)),
    );
    w(
        &mut o,
        format!("Codex resumable:  {}", count(Agent::Codex, false)),
    );
    w(
        &mut o,
        format!("Codex subagent:   {}", count(Agent::Codex, true)),
    );
    if homes.has(Agent::Pi) {
        w(
            &mut o,
            format!("pi sessions:      {}", count(Agent::Pi, false)),
        );
    }
    if homes.has(Agent::OpenCode) {
        w(
            &mut o,
            format!("opencode resumable: {}", count(Agent::OpenCode, false)),
        );
        w(
            &mut o,
            format!("opencode subtask:   {}", count(Agent::OpenCode, true)),
        );
    }
    w(&mut o, format!("Total valid:      {}", disk.files.len()));
    if disk.invalid > 0 {
        w(
            &mut o,
            format!("Skipped invalid:  {} (no resumable messages)", disk.invalid),
        );
    }
    let home_strs: HashSet<String> = Agent::ALL
        .into_iter()
        .filter(|a| homes.has(*a))
        .map(|a| absolute(homes.home(a)).to_string_lossy().into_owned())
        .collect();
    let mine: Vec<&Row> = rows
        .iter()
        .filter(|r| home_strs.contains(&r.home))
        .collect();
    let ids: HashSet<&str> = mine.iter().map(|r| r.session_id.as_str()).collect();
    let indexed: HashMap<&str, &Row> = mine.iter().map(|r| (r.file_path.as_str(), *r)).collect();
    w(&mut o, "\n=== Index ===".into());
    w(&mut o, format!("Total documents: {}", rows.len()));
    w(
        &mut o,
        format!("Unique session IDs: {} (for specified homes)", ids.len()),
    );
    w(&mut o, format!("Total file paths:   {}", indexed.len()));
    let on_disk: HashMap<&str, &DiskFile> =
        disk.files.iter().map(|f| (f.path.as_str(), f)).collect();
    // Not indexed: either deliberately skipped by the indexer (recorded in
    // the state: empty, helper, cwd inside a home) or genuinely missing.
    let (skipped, missing): (Vec<&DiskFile>, Vec<&DiskFile>) = disk
        .files
        .iter()
        .filter(|f| !indexed.contains_key(f.path.as_str()))
        .partition(|f| state.contains_key(&f.path));
    let mut orphaned: Vec<&Row> = mine
        .iter()
        .filter(|r| {
            !on_disk.contains_key(r.file_path.as_str()) && !Path::new(&r.file_path).exists()
        })
        .copied()
        .collect();
    orphaned.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    let in_sync = disk.files.len() - missing.len() - skipped.len();
    w(&mut o, "\n=== Reconciliation (by file path) ===".into());
    w(&mut o, format!("Files on disk:  {}", disk.files.len()));
    w(&mut o, format!("Files indexed:  {}", indexed.len()));
    w(&mut o, format!("In sync:        {in_sync}"));
    if !skipped.is_empty() {
        w(
            &mut o,
            format!(
                "Not indexable:  {} (empty, helper or cwd inside a home)",
                skipped.len()
            ),
        );
    }
    if missing.is_empty() && orphaned.is_empty() {
        w(&mut o, "✅ Index is fully in sync with filesystem".into());
    }
    if !missing.is_empty() {
        let subs = missing.iter().filter(|f| f.subagent).count();
        w(&mut o, format!("❌ Missing from index: {}", missing.len()));
        w(
            &mut o,
            format!("   ({} resumable, {subs} subagent)", missing.len() - subs),
        );
        for f in missing.iter().take(3) {
            w(
                &mut o,
                format!(
                    "   - {} ({}, {})",
                    file_name(&f.path),
                    f.agent,
                    kind(f.subagent)
                ),
            );
        }
        if missing.len() > 3 {
            w(&mut o, format!("   ... and {} more", missing.len() - 3));
        }
    }
    if !orphaned.is_empty() {
        let subs = orphaned.iter().filter(|r| r.is_sidechain).count();
        w(
            &mut o,
            format!("⚠️  Orphaned in index (file gone): {}", orphaned.len()),
        );
        w(
            &mut o,
            format!("   ({} resumable, {subs} subagent)", orphaned.len() - subs),
        );
        for r in orphaned.iter().take(3) {
            w(
                &mut o,
                format!("   - {} ({})", file_name(&r.file_path), r.agent),
            );
        }
        if orphaned.len() > 3 {
            w(&mut o, format!("   ... and {} more", orphaned.len() - 3));
        }
    }
    if let Some(c) = cwd {
        let docs: Vec<&Row> = rows.iter().filter(|r| r.cwd == c).collect();
        let uniq: HashSet<&str> = docs.iter().map(|r| r.session_id.as_str()).collect();
        w(&mut o, format!("\nWith cwd '{c}':"));
        w(&mut o, format!("  Documents: {}", docs.len()));
        w(&mut o, format!("  Unique sessions: {}", uniq.len()));
    }
    w(&mut o, "\nTop 5 cwds:".into());
    for (n, c) in top_counts(rows.iter().map(|r| r.cwd.as_str()), 5) {
        let shown = if c.chars().count() > 50 {
            let tail: String = c.chars().skip(c.chars().count() - 50).collect();
            format!("...{tail}")
        } else {
            c.to_string()
        };
        w(&mut o, format!("  {n:4} | {shown}"));
    }
    w(&mut o, "\nHomes:".into());
    for (n, h) in top_counts(rows.iter().map(|r| r.home.as_str()), 10) {
        w(
            &mut o,
            format!("  {n:4} | {}", if h.is_empty() { "(empty)" } else { h }),
        );
    }
    o
}

/// Most frequent values, count desc then value asc.
fn top_counts<'a>(vals: impl Iterator<Item = &'a str>, n: usize) -> Vec<(usize, &'a str)> {
    let mut m: BTreeMap<&str, usize> = BTreeMap::new();
    for v in vals {
        *m.entry(v).or_default() += 1;
    }
    let mut v: Vec<(usize, &str)> = m.into_iter().map(|(k, c)| (c, k)).collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    v.truncate(n);
    v
}

/// `mnemoloom search [QUERY] ...`: TUI, or JSONL with `--json`.
pub fn search(global: &GlobalArgs, args: &SearchArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?.canonical();
    let launch_cwd = std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let filters = filters_from_args(args, &homes, launch_cwd)?;
    let dir = store::default_index_dir();
    let query = args.query.clone().unwrap_or_default();
    if !args.json {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            anyhow::bail!("search needs a terminal; use --json");
        }
        return tui::run(tui::Launch {
            global: global.clone(),
            homes,
            index_dir: dir,
            filters,
            query,
            by_time: args.by_time,
            limit: args.num_results,
        });
    }
    // Don't wait for another process's build unless there is no index yet.
    let wait = if store::index_exists(&dir) {
        store::LockWait::Try
    } else {
        store::LockWait::Block
    };
    match build::update(&dir, &homes, None, wait) {
        Ok(s) if s.busy => {
            eprintln!("Note: another mnemoloom process is updating the index; searching it as is.")
        }
        Ok(_) => {}
        Err(e) => eprintln!("Warning: index update failed: {e:#}"),
    }
    let Some(cat) = Catalog::open(&dir, &homes)? else {
        println!("[]");
        return Ok(ExitCode::SUCCESS);
    };
    if cat.rows.is_empty() {
        println!("[]");
        return Ok(ExitCode::SUCCESS);
    }
    let q = (!query.trim().is_empty())
        .then(|| query::run_query(&cat, &query, &filters, SystemTime::now()));
    let live = filters.live_only.then(|| {
        live::scan(&live::candidates(&cat.rows))
            .into_keys()
            .collect::<HashSet<_>>()
    });
    let sel = query::select(
        &cat,
        &filters,
        q.as_ref(),
        args.by_time,
        args.num_results,
        live.as_ref(),
    );
    let searcher = cat.searcher();
    let mut out = io::stdout().lock();
    for i in sel {
        let row = &cat.rows[i];
        let snip = q.as_ref().map(|q| q.snippet(&cat, &searcher, row.doc));
        writeln!(out, "{}", json_line(row, snip.as_ref()))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Build [`Filters`] from CLI flags.
pub fn filters_from_args(
    args: &SearchArgs,
    homes: &Homes,
    launch_cwd: String,
) -> anyhow::Result<Filters> {
    let mut f = Filters::new(homes, launch_cwd);
    f.global = args.global;
    f.branch = args.branch.clone().filter(|b| !b.is_empty());
    if let Some(spec) = &args.dir {
        let (dir, branch) = split_dir_branch(spec);
        if !dir.is_empty() {
            f.dir = Some(resolve_dir(dir, &f.launch_cwd));
            f.global = false;
        }
        if let Some(b) = branch {
            f.branch = Some(b.to_string());
        }
    }
    f.include_original = !args.no_original;
    f.include_sub = args.sub_agent;
    f.include_exec = args.exec_runs;
    f.include_trimmed = !args.no_trimmed;
    f.include_continued = !args.no_rollover;
    f.live_only = args.live;
    f.min_lines = args.min_lines.filter(|&n| n > 0).map(|n| n as i64);
    f.agents = match args.agent {
        SearchAgent::Claude => Agents::only(Agent::Claude),
        SearchAgent::Codex => Agents::only(Agent::Codex),
        SearchAgent::Pi => Agents::only(Agent::Pi),
        SearchAgent::OpenCode => Agents::only(Agent::OpenCode),
        SearchAgent::All => Agents::ALL,
    };
    let date = |flag: &str, v: &Option<String>| -> anyhow::Result<_> {
        v.as_deref()
            .map(|s| {
                parse_date_now(s).ok_or_else(|| {
                    Exit::fail(format!(
                        "Error: Invalid date for {flag}: {s} (try YYYY-MM-DD, MM/DD/YY or YYYY-MM-DDTHH:MM)"
                    ))
                })
            })
            .transpose()
            .map_err(Into::into)
    };
    f.after = date("--after", &args.after)?;
    f.before = date("--before", &args.before)?;
    Ok(f)
}

/// `dir[:branch]`: split on the last `:` unless the whole spec is an
/// existing path. Branches may contain `/`.
pub fn split_dir_branch(spec: &str) -> (&str, Option<&str>) {
    match spec.rsplit_once(':') {
        Some(_) if expand_tilde(Path::new(spec)).exists() => (spec, None),
        Some((d, b)) => (d, Some(b).filter(|b| !b.is_empty())),
        None => (spec, None),
    }
}

/// `~` expansion, relative to `base`, symlinks resolved when it exists.
pub fn resolve_dir(dir: &str, base: &str) -> String {
    let p = expand_tilde(Path::new(dir));
    let p = if p.is_absolute() {
        p
    } else {
        Path::new(base).join(p)
    };
    let p = p.canonicalize().unwrap_or(p);
    let s = p.to_string_lossy();
    match s.trim_end_matches('/') {
        "" => "/".to_string(),
        t => t.to_string(),
    }
}

/// One `--json` line: fields in alphabetical order.
pub fn json_line(r: &Row, snippet: Option<&Snippet>) -> String {
    let v = json!({
        "agent": r.agent.as_str(),
        "branch": r.branch,
        "created": r.created,
        "custom_title": r.custom_title,
        "cwd": r.cwd,
        "derivation_type": r.derivation_type,
        "file_path": r.file_path,
        "first_msg": r.first_msg(),
        "is_exec_run": r.is_exec_run,
        "is_sidechain": r.is_sidechain,
        "last_msg": r.last_msg_content,
        "lines": r.lines,
        "modified": r.modified,
        "project": r.project,
        "session_id": r.session_id,
        "snippet": snippet.map_or(Value::Null, |s| Value::String(s.text.clone())),
    });
    v.to_string()
}

#[cfg(test)]
mod tests;
