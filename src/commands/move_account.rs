//! `mnemoloom move-account SESSION --to DIR [--from DIR] [--agent] [--keep]`:
//! move a transcript (plus Claude sidecar dir / Codex thread names) between
//! two config dirs ("accounts"), keeping its relative location.

use std::collections::HashSet;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use serde_json::Value;

use super::files::copy2;
use crate::cli::{GlobalArgs, MoveAccountArgs};
use crate::exit::Exit;
use crate::session::discover::{codex_thread_names, scan_paths};
use crate::session::homes::{agent_home, expand_tilde, profile_homes, user_home};
use crate::session::launch::shell_quote;
use crate::session::meta::{Depth, detect_agent_from_content};
use crate::session::resolve::{MatchKind, SessionRecord, choose_record};
use crate::session::timefmt::iso_local_offset;
use crate::session::transcript::{FileSnapshot, temp_sibling, write_atomic_with};
use crate::session::{Agent, absolute, path_session_uuid};

fn err(msg: impl std::fmt::Display) -> anyhow::Error {
    Exit::fail(format!("Error: {msg}")).into()
}

/// `projects/` only -> Claude, `sessions/` only -> Codex.
pub fn detect_home_kind(home: &Path) -> Option<Agent> {
    let home = expand_tilde(home);
    match (
        home.join("projects").is_dir(),
        home.join("sessions").is_dir(),
    ) {
        (true, false) => Some(Agent::Claude),
        (false, true) => Some(Agent::Codex),
        _ => None,
    }
}

fn required_subdir(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "projects",
        Agent::Codex | Agent::Pi => "sessions",
        Agent::OpenCode => "opencode.db",
    }
}

/// Source-home candidates: env home, default home, sorted `~/.claude*` /
/// `~/.codex*` siblings having the required subdir, excluding `to`.
pub fn candidate_source_homes(agent: Agent, to: &Path) -> Vec<PathBuf> {
    let env = match agent {
        Agent::Claude => "CLAUDE_CONFIG_DIR",
        Agent::Codex => "CODEX_HOME",
        Agent::Pi => "PI_CODING_AGENT_DIR",
        Agent::OpenCode => "XDG_DATA_HOME",
    };
    let mut raw = profile_homes(&user_home(), agent);
    raw[1..].sort();
    if let Some(v) = std::env::var_os(env).filter(|v| !v.is_empty()) {
        raw.insert(0, expand_tilde(Path::new(&v)));
    }
    let to_resolved = fs::canonicalize(to).unwrap_or_else(|_| absolute(to));
    let mut seen = std::collections::HashSet::new();
    raw.into_iter()
        .filter(|c| c.join(required_subdir(agent)).is_dir())
        .filter(|c| {
            let r = fs::canonicalize(c).unwrap_or_else(|_| absolute(c));
            r != to_resolved && seen.insert(r)
        })
        .collect()
}

#[derive(Debug, Clone)]
struct Candidate {
    path: PathBuf,
    session_id: String,
    title: String,
}

fn candidates_in(home: &Path, agent: Agent) -> Vec<Candidate> {
    match agent {
        Agent::Claude => {
            let paths: Vec<(Agent, PathBuf)> =
                crate::session::discover::claude_session_paths(home, false)
                    .into_iter()
                    .filter(|p| {
                        !p.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .starts_with("agent-")
                    })
                    .map(|p| (agent, p))
                    .collect();
            scan_paths(paths, Depth::Full)
                .into_iter()
                .map(|sf| Candidate {
                    session_id: sf
                        .path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    title: sf.name.unwrap_or_default(),
                    path: sf.path,
                })
                .collect()
        }
        Agent::Codex => {
            let names = codex_thread_names(home);
            crate::session::discover::codex_session_paths(home)
                .into_iter()
                .map(|p| {
                    let id = path_session_uuid(&p);
                    Candidate {
                        title: names.get(&id.to_lowercase()).cloned().unwrap_or_default(),
                        session_id: id,
                        path: p,
                    }
                })
                .collect()
        }
        // `run` refuses these agents before looking for candidates.
        Agent::Pi | Agent::OpenCode => Vec::new(),
    }
}

fn candidate_match(c: &Candidate, query: &str) -> Option<(u8, MatchKind)> {
    let q = query.to_lowercase();
    let id = c.session_id.to_lowercase();
    let title = c.title.to_lowercase();
    let file = c
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    if id == q {
        Some((0, MatchKind::Id))
    } else if title == q {
        Some((1, MatchKind::Name))
    } else if id.starts_with(&q) {
        Some((2, MatchKind::PartialId))
    } else if id.contains(&q) {
        Some((3, MatchKind::IdSubstring))
    } else if file.contains(&q) {
        Some((4, MatchKind::Filename))
    } else if !title.is_empty() && title.contains(&q) {
        Some((5, MatchKind::Name))
    } else {
        None
    }
}

struct Match {
    home: PathBuf,
    cand: Candidate,
    kind: MatchKind,
}

fn transcript_cwd(path: &Path) -> String {
    use std::io::BufRead;
    let Ok(f) = fs::File::open(path) else {
        return String::new();
    };
    let mut reader = std::io::BufReader::new(f);
    let mut buf = Vec::new();
    while reader.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
        if let Ok(v) = serde_json::from_slice::<Value>(&buf) {
            let pick = |x: Option<&Value>| {
                x.filter(|c| !c.is_null() && c.as_str() != Some("") && c.as_bool() != Some(false))
                    .map(|c| c.as_str().map_or_else(|| c.to_string(), str::to_string))
            };
            let payload_cwd = v
                .get("payload")
                .filter(|p| p.is_object())
                .and_then(|p| p.get("cwd"));
            if let Some(c) = pick(v.get("cwd")).or_else(|| pick(payload_cwd)) {
                return c;
            }
        }
        buf.clear();
    }
    String::new()
}

fn record_of(m: &Match, agent: Agent) -> SessionRecord {
    let modified_time = fs::metadata(&m.cand.path)
        .and_then(|md| md.modified())
        .unwrap_or(std::time::UNIX_EPOCH);
    let cwd = transcript_cwd(&m.cand.path);
    SessionRecord {
        agent,
        session_id: m.cand.session_id.clone(),
        name: (!m.cand.title.is_empty()).then(|| m.cand.title.clone()),
        directory: (!cwd.is_empty()).then_some(cwd),
        home: m.home.clone(),
        session_file: m.cand.path.clone(),
        matched_by: Some(m.kind),
        modified: iso_local_offset(modified_time),
        modified_time,
        archived: false,
        eligible: true,
        content_id: None,
        git_branch: None,
    }
}

pub fn run(_global: &GlobalArgs, args: &MoveAccountArgs) -> anyhow::Result<ExitCode> {
    let to = expand_tilde(&args.to);
    let agent = match args.agent {
        Some(a) => a.into(),
        None => {
            let to_kind = detect_home_kind(&to);
            let from_kind = args.from.as_deref().and_then(detect_home_kind);
            if let (Some(t), Some(f)) = (to_kind, from_kind)
                && t != f
            {
                return Err(err(format!(
                    "source looks like a {f} home but target looks like a {t} home"
                )));
            }
            to_kind.or(from_kind).ok_or_else(|| {
                err(format!(
                    "cannot tell whether {} is a Claude or Codex config dir; pass --agent claude|codex",
                    to.display()
                ))
            })?
        }
    };
    agent.require_rewritable("move-account")?;
    let sub = required_subdir(agent);
    if !to.join(sub).is_dir() {
        return Err(err(format!(
            "{} does not look like a {agent} config dir (no {sub}/ subdir)",
            to.display()
        )));
    }
    let homes: Vec<PathBuf> = match &args.from {
        Some(f) => {
            let from = agent_home(agent, Some(f));
            let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| absolute(p));
            if canon(&from) == canon(&to) {
                return Err(err(format!(
                    "source and target account dirs are the same: {}",
                    from.display()
                )));
            }
            vec![from]
        }
        None => {
            let h = candidate_source_homes(agent, &to);
            if h.is_empty() {
                return Err(err(format!(
                    "no local {agent} config dirs found to search; pass --from"
                )));
            }
            h
        }
    };

    let matches = match direct_match(&args.session, &homes, agent)? {
        Some(m) => vec![m],
        None => {
            let mut all: Vec<(u8, Match)> = homes
                .iter()
                .flat_map(|h| {
                    candidates_in(h, agent).into_iter().filter_map(|c| {
                        candidate_match(&c, &args.session).map(|(tier, kind)| {
                            (
                                tier,
                                Match {
                                    home: h.clone(),
                                    cand: c,
                                    kind,
                                },
                            )
                        })
                    })
                })
                .collect();
            let Some(best) = all.iter().map(|(t, _)| *t).min() else {
                return Err(err(if args.from.is_some() {
                    format!(
                        "no session matching '{}' in {}",
                        args.session,
                        homes[0].display()
                    )
                } else {
                    let searched: Vec<String> =
                        homes.iter().map(|h| h.display().to_string()).collect();
                    format!(
                        "no session matching '{}' in any of: {}",
                        args.session,
                        searched.join(", ")
                    )
                }));
            };
            all.retain(|(t, _)| *t == best);
            all.into_iter().map(|(_, m)| m).collect()
        }
    };
    let selected = if matches.len() == 1 {
        matches.into_iter().next().expect("one")
    } else {
        select(&args.session, matches, agent)?
    };

    let verb = if args.keep { "Copying" } else { "Moving" };
    let label = if selected.cand.title.is_empty() {
        String::new()
    } else {
        format!(" ({})", selected.cand.title)
    };
    println!("{verb} {agent} session {}{label}", selected.cand.session_id);
    println!("  from account: {}", selected.home.display());
    println!("  to account:   {}", to.display());
    let result =
        move_between(agent, &selected.cand.path, &selected.home, &to, args.keep).map_err(err)?;
    println!("  transcript:   {}", result.dest_file.display());
    if result.sidecar_moved {
        println!(
            "  sidecar dir:  {}",
            result.dest_file.with_extension("").display()
        );
    }
    if result.index_moved {
        println!("  thread name:  moved in session_index.jsonl");
    }
    if args.keep {
        println!("  source left in place (--keep)");
    }
    println!("\nResume with:");
    let to_q = shell_quote(
        &fs::canonicalize(&to)
            .unwrap_or(to.clone())
            .to_string_lossy(),
    );
    let cd = if result.cwd.is_empty() {
        String::new()
    } else {
        format!("cd {} && ", shell_quote(&result.cwd))
    };
    match agent {
        Agent::Claude => println!(
            "  {cd}CLAUDE_CONFIG_DIR={to_q} claude --resume {}",
            result.session_id
        ),
        Agent::Codex => println!("  {cd}CODEX_HOME={to_q} codex resume {}", result.session_id),
        Agent::Pi | Agent::OpenCode => {}
    }
    Ok(ExitCode::SUCCESS)
}

fn direct_match(session: &str, homes: &[PathBuf], agent: Agent) -> anyhow::Result<Option<Match>> {
    let path = absolute(&expand_tilde(Path::new(session)));
    if !path.is_file() {
        return Ok(None);
    }
    let detected = detect_agent_from_content(&path);
    if detected != Some(agent) {
        let label = detected.map_or("unknown".to_string(), |a| a.to_string());
        return Err(err(format!(
            "session file is {label}, but the account move is configured for {agent}: {}",
            path.display()
        )));
    }
    let owners: Vec<&PathBuf> = homes
        .iter()
        .filter(|h| path.starts_with(absolute(&expand_tilde(h))))
        .collect();
    let [home] = owners.as_slice() else {
        return Err(err(format!(
            "session path must be inside exactly one eligible source home: {}",
            path.display()
        )));
    };
    let cand = candidates_in(home, agent)
        .into_iter()
        .find(|c| absolute(&c.path) == path)
        .ok_or_else(|| {
            err(format!(
                "session path is not an eligible transcript: {}",
                path.display()
            ))
        })?;
    Ok(Some(Match {
        home: (*home).clone(),
        cand,
        kind: MatchKind::Filename,
    }))
}

fn select(session: &str, mut matches: Vec<Match>, agent: Agent) -> anyhow::Result<Match> {
    let mut records: Vec<(SessionRecord, usize)> = matches
        .iter()
        .enumerate()
        .map(|(i, m)| (record_of(m, agent), i))
        .collect();
    records.sort_by_key(|r| std::cmp::Reverse(r.0.modified_time));
    if !std::io::stdin().is_terminal() {
        let mut e = std::io::stderr().lock();
        writeln!(e, "Error: multiple sessions match '{session}':")?;
        for (r, _) in &records {
            let label = r
                .name
                .as_deref()
                .map(|n| format!(" ({n})"))
                .unwrap_or_default();
            writeln!(e, "  {}: {}{label}", r.home.display(), r.session_id)?;
        }
        writeln!(
            e,
            "Use a more specific ID or name, or disambiguate with --from."
        )?;
        return Err(Exit::code(1).into());
    }
    let recs: Vec<SessionRecord> = records.iter().map(|(r, _)| r.clone()).collect();
    match choose_record(
        session,
        &recs,
        recs.len(),
        "Which source session do you want to move?",
        true,
    )? {
        Some(i) => Ok(matches.swap_remove(records[i].1)),
        None => Err(Exit::fail("Session selection cancelled.").into()),
    }
}

/// Outcome of [`move_between`].
#[derive(Debug)]
pub struct MoveResult {
    pub dest_file: PathBuf,
    pub sidecar_moved: bool,
    pub index_moved: bool,
    pub session_id: String,
    pub cwd: String,
}

/// Copy (and unless `keep`, remove) one transcript from `from` to `to`,
/// keeping its path relative to the home. Claude also moves the sidecar
/// dir; Codex also moves `session_index.jsonl` entries.
pub fn move_between(
    agent: Agent,
    file: &Path,
    from: &Path,
    to: &Path,
    keep: bool,
) -> anyhow::Result<MoveResult> {
    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| absolute(&expand_tilde(p)));
    let (file, from, to) = (canon(file), canon(from), canon(to));
    let Ok(rel) = file.strip_prefix(&from) else {
        bail!(
            "Session {} is not inside source home {}",
            file.display(),
            from.display()
        );
    };
    let dest = to.join(rel);
    if dest.exists() {
        bail!(
            "Session already exists in target account: {}",
            dest.display()
        );
    }
    let session_id = path_session_uuid(&file);
    let sidecar_src = file.with_extension("");
    let sidecar_dest = dest.with_extension("");
    match agent {
        Agent::Claude => {
            if sidecar_src.is_dir() && sidecar_dest.exists() {
                bail!(
                    "Sidecar dir already exists in target account: {}",
                    sidecar_dest.display()
                );
            }
        }
        Agent::Codex => {
            let suffix = format!("{session_id}.jsonl");
            let clash = crate::session::discover::codex_session_paths(&to)
                .into_iter()
                .find(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().ends_with(&suffix))
                });
            if let Some(c) = clash {
                bail!(
                    "Session {session_id} already exists in target account: {}",
                    c.display()
                );
            }
        }
        Agent::Pi | Agent::OpenCode => agent.require_rewritable("move-account")?,
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let snap = FileSnapshot::of(&file).with_context(|| format!("reading {}", file.display()))?;
    let with_sidecar = agent == Agent::Claude && sidecar_src.is_dir();
    let staged = Staged {
        file: temp_sibling(&dest),
        sidecar: with_sidecar.then(|| temp_sibling(&sidecar_dest)),
    };
    if let Err(e) = stage(&file, &sidecar_src, &staged, &snap) {
        staged.discard();
        return Err(e);
    }
    if let Some(side) = &staged.sidecar
        && let Err(e) = fs::rename(side, &sidecar_dest)
    {
        staged.discard();
        return Err(e).with_context(|| format!("placing {}", sidecar_dest.display()));
    }
    if let Err(e) = fs::rename(&staged.file, &dest) {
        staged.discard();
        if with_sidecar {
            let _ = fs::remove_dir_all(&sidecar_dest);
        }
        return Err(e).with_context(|| format!("placing {}", dest.display()));
    }
    let index_moved =
        agent == Agent::Codex && transfer_index_entries(&from, &to, &session_id, keep)?;
    let cwd = transcript_cwd(&dest);
    if !keep {
        if let Err(e) = snap.ensure_unchanged(&file) {
            let _ = fs::remove_file(&dest);
            if with_sidecar {
                let _ = fs::remove_dir_all(&sidecar_dest);
            }
            return Err(e.context("source untouched; target copy removed"));
        }
        fs::remove_file(&file).with_context(|| format!("removing {}", file.display()))?;
        if with_sidecar {
            fs::remove_dir_all(&sidecar_src)
                .with_context(|| format!("removing {}", sidecar_src.display()))?;
        }
    }
    Ok(MoveResult {
        dest_file: dest,
        sidecar_moved: with_sidecar,
        index_moved,
        session_id,
        cwd,
    })
}

/// Temp names in the target account; renamed into place only when
/// everything was copied.
struct Staged {
    file: PathBuf,
    sidecar: Option<PathBuf>,
}

impl Staged {
    fn discard(&self) {
        let _ = fs::remove_file(&self.file);
        if let Some(s) = &self.sidecar {
            let _ = fs::remove_dir_all(s);
        }
    }
}

fn stage(
    file: &Path,
    sidecar_src: &Path,
    staged: &Staged,
    snap: &FileSnapshot,
) -> anyhow::Result<()> {
    copy2(file, &staged.file).with_context(|| format!("copying {}", file.display()))?;
    let copied = fs::metadata(&staged.file)?.len();
    if copied != snap.size || snap.ensure_unchanged(file).is_err() {
        bail!(
            "Copy verification failed for {} — the source changed during the copy (session still running?); source untouched",
            file.display()
        );
    }
    if let Some(side) = &staged.sidecar {
        copy_tree(sidecar_src, side, &mut HashSet::new())?;
    }
    Ok(())
}

/// Recursive copy following symlinks (Python `copytree(symlinks=False)`);
/// a symlink cycle is an error.
fn copy_tree(src: &Path, dest: &Path, ancestors: &mut HashSet<PathBuf>) -> anyhow::Result<()> {
    let canon = fs::canonicalize(src).with_context(|| format!("reading {}", src.display()))?;
    if !ancestors.insert(canon.clone()) {
        bail!("symlink loop at {}", src.display());
    }
    fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    for e in fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let p = e?.path();
        let target = dest.join(p.file_name().unwrap_or_default());
        let md = fs::metadata(&p).with_context(|| format!("reading {}", p.display()))?;
        if md.is_dir() {
            copy_tree(&p, &target, ancestors)?;
        } else {
            copy2(&p, &target).with_context(|| format!("copying {}", p.display()))?;
        }
    }
    ancestors.remove(&canon);
    Ok(())
}

fn is_index_entry_for(line: &[u8], session_id: &str) -> bool {
    serde_json::from_slice::<Value>(line)
        .ok()
        .and_then(|v| {
            v.get("id")
                .and_then(Value::as_str)
                .map(|id| id.trim().eq_ignore_ascii_case(session_id))
        })
        .unwrap_or(false)
}

/// Move a session's `session_index.jsonl` lines from `from` to `to`, byte
/// for byte. The source rewrite is atomic and retried if Codex appended to
/// the index meanwhile.
fn transfer_index_entries(
    from: &Path,
    to: &Path,
    session_id: &str,
    keep: bool,
) -> anyhow::Result<bool> {
    let src = from.join("session_index.jsonl");
    let Ok(bytes) = fs::read(&src) else {
        return Ok(false);
    };
    let matched: Vec<u8> = bytes
        .split_inclusive(|&b| b == b'\n')
        .filter(|l| is_index_entry_for(l, session_id))
        .flat_map(|l| {
            let mut l = l.to_vec();
            if l.last() != Some(&b'\n') {
                l.push(b'\n');
            }
            l
        })
        .collect();
    if matched.is_empty() {
        return Ok(false);
    }
    let dest = to.join("session_index.jsonl");
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&dest)
        .with_context(|| format!("opening {}", dest.display()))?;
    if last_byte(&mut f)?.is_some_and(|b| b != b'\n') {
        f.write_all(b"\n")?;
    }
    f.write_all(&matched)?;
    f.sync_all()?;
    if keep {
        return Ok(true);
    }
    let mut last_err = None;
    for _ in 0..3 {
        let snap = FileSnapshot::of(&src)?;
        let current = fs::read(&src)?;
        let remaining: Vec<u8> = current
            .split_inclusive(|&b| b == b'\n')
            .filter(|l| !is_index_entry_for(l, session_id))
            .flatten()
            .copied()
            .collect();
        match write_atomic_with(&src, &remaining, Some(&src), None, &|| {
            snap.ensure_unchanged(&src)
        }) {
            Ok(()) => return Ok(true),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("could not rewrite {}", src.display())))
}

fn last_byte(f: &mut fs::File) -> std::io::Result<Option<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    if f.metadata()?.len() == 0 {
        return Ok(None);
    }
    f.seek(SeekFrom::End(-1))?;
    let mut b = [0u8; 1];
    f.read_exact(&mut b)?;
    Ok(Some(b[0]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;

    #[test]
    fn claude_move_with_sidecar() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join(".claude-a"), d.path().join(".claude-b"));
        let f = write_claude(&a, CLAUDE_ID, "/w/p", &claude_lines(CLAUDE_ID, "/w/p"));
        write_raw(
            &f.with_extension("").join("subagents/agent-1.jsonl"),
            &["{}".into()],
        );
        fs::create_dir_all(b.join("projects")).unwrap();
        assert_eq!(detect_home_kind(&b), Some(Agent::Claude));
        let r = move_between(Agent::Claude, &f, &a, &b, false).unwrap();
        assert!(r.sidecar_moved && !f.exists() && !f.with_extension("").exists());
        assert!(
            r.dest_file
                .with_extension("")
                .join("subagents/agent-1.jsonl")
                .is_file()
        );
        assert_eq!(r.cwd, "/w/p");
        // Moving back over an existing copy is refused.
        let f2 = write_claude(&a, CLAUDE_ID, "/w/p", &claude_lines(CLAUDE_ID, "/w/p"));
        assert!(
            move_between(Agent::Claude, &f2, &a, &b, true)
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
    }

    #[test]
    fn claude_move_follows_symlinks_and_is_transactional() {
        // Regression: symlinked side dirs are copied as real dirs, and a failed
        // move leaves the source untouched.
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join(".claude-a"), d.path().join(".claude-b"));
        let f = write_claude(&a, CLAUDE_ID, "/w/p", &claude_lines(CLAUDE_ID, "/w/p"));
        let side = f.with_extension("");
        let outside = d.path().join("outside");
        write_raw(&outside.join("x.txt"), &["hi".into()]);
        fs::create_dir_all(&side).unwrap();
        std::os::unix::fs::symlink(&outside, side.join("linked")).unwrap();
        fs::create_dir_all(b.join("projects")).unwrap();
        let r = move_between(Agent::Claude, &f, &a, &b, false).unwrap();
        let moved = r.dest_file.with_extension("");
        assert!(
            moved.join("linked/x.txt").is_file(),
            "symlinked dir copied as dir"
        );
        assert!(!f.exists());

        // A dangling link fails the move cleanly: nothing left in the target.
        let f = write_claude(&a, CLAUDE_ID2, "/w/p", &claude_lines(CLAUDE_ID2, "/w/p"));
        let side = f.with_extension("");
        fs::create_dir_all(&side).unwrap();
        std::os::unix::fs::symlink(d.path().join("nope"), side.join("dangling")).unwrap();
        let before: Vec<_> = walk(&b);
        let err = move_between(Agent::Claude, &f, &a, &b, false).unwrap_err();
        assert!(format!("{err:#}").contains("dangling"), "{err:#}");
        assert_eq!(walk(&b), before, "target unchanged");
        assert!(f.exists() && side.is_dir(), "source untouched");
        // ...so a retry after fixing the source succeeds.
        fs::remove_file(side.join("dangling")).unwrap();
        assert!(move_between(Agent::Claude, &f, &a, &b, false).is_ok());
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                out.push(e.path());
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn index_transfer_preserves_bytes() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let mut src = format!("{{\"id\":\"{CODEX_ID}\",\"thread_name\":\"t\"}}\n").into_bytes();
        src.extend_from_slice(b"{\"id\":\"o\",\"thread_name\":\"\xff bad utf8\"}\n");
        fs::write(a.join("session_index.jsonl"), &src).unwrap();
        assert!(transfer_index_entries(&a, &b, CODEX_ID, false).unwrap());
        assert_eq!(
            fs::read(a.join("session_index.jsonl")).unwrap(),
            b"{\"id\":\"o\",\"thread_name\":\"\xff bad utf8\"}\n"
        );
        assert_eq!(fs::read_dir(&a).unwrap().count(), 1, "no temp files left");
    }

    #[test]
    fn codex_move_with_index() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join(".codex-a"), d.path().join(".codex-b"));
        let f = write_codex(
            &a,
            CODEX_ID,
            "2026-05-07T12-57-30",
            &codex_lines(CODEX_ID, "/w", "cli"),
        );
        fs::write(a.join("session_index.jsonl"), format!("{{\"id\":\"{CODEX_ID}\",\"thread_name\":\"t\"}}\n{{\"id\":\"other\",\"thread_name\":\"o\"}}\n")).unwrap();
        fs::create_dir_all(b.join("sessions")).unwrap();
        fs::write(
            b.join("session_index.jsonl"),
            "{\"id\":\"z\",\"thread_name\":\"z\"}",
        )
        .unwrap();
        let r = move_between(Agent::Codex, &f, &a, &b, true).unwrap();
        assert!(r.index_moved && f.exists(), "--keep leaves the source");
        let idx = fs::read_to_string(b.join("session_index.jsonl")).unwrap();
        assert!(idx.starts_with("{\"id\":\"z\",\"thread_name\":\"z\"}\n{\"id\":\""));
        assert_eq!(
            fs::read_to_string(a.join("session_index.jsonl"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert_eq!(
            r.dest_file,
            fs::canonicalize(&b)
                .unwrap()
                .join("sessions/2026/05/07")
                .join(f.file_name().unwrap())
        );
    }

    #[test]
    fn tiers() {
        let c = Candidate {
            path: PathBuf::from("/x/rollout-2026-01-01T00-00-00-abcd.jsonl"),
            session_id: "abcd".into(),
            title: "My Thing".into(),
        };
        assert_eq!(candidate_match(&c, "ABCD").unwrap().0, 0);
        assert_eq!(candidate_match(&c, "my thing").unwrap().0, 1);
        assert_eq!(candidate_match(&c, "ab").unwrap().0, 2);
        assert_eq!(candidate_match(&c, "cd").unwrap().0, 3);
        assert_eq!(candidate_match(&c, "2026-01").unwrap().0, 4);
        assert_eq!(candidate_match(&c, "thin").unwrap().0, 5);
    }
}
