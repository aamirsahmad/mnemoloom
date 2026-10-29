//! `trim-in-place`: trim a Claude session file
//! in place, keeping its id. A timestamped backup copy
//! (`<stem>.pre-trim-<YYYYMMDD-HHMMSS>[-N].jsonl.bak`) is referenced by the
//! truncation notices; the trimmed content is swapped in atomically under a
//! stat guard, and bytes appended to the old inode around the swap are
//! spliced in after the trimmed content (and mirrored into the backup).

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};

use super::{
    MIN_TOKEN_SAVINGS, TrimOptions, TrimStats, detect_agent_strict, group, parse_tools, trim_lines,
};
use crate::cli::{GlobalArgs, TrimInPlaceArgs};
use crate::commands::files::copy2;
use crate::commands::thousands;
use crate::session::homes::expand_tilde;
use crate::session::resolve::{exact_claude_filename, resolve_query};
use crate::session::transcript::{FileSnapshot, WriteStyle};
use crate::session::{self, Agent, ResolveOptions, Transcript, pyjson, resolve};

const MAX_ATTEMPTS: usize = 3;
const MERGE_QUIET: Duration = Duration::from_millis(250);
const MERGE_POLL: Duration = Duration::from_millis(20);
const MERGE_MAX: Duration = Duration::from_secs(5);

/// Outcome of [`trim_session_in_place`] (the `--json` dict).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InPlaceResult {
    pub applied: bool,
    pub dry_run: bool,
    pub nothing_to_trim: bool,
    pub stats: TrimStats,
    pub backup_file: Option<PathBuf>,
    pub session_file: PathBuf,
    pub size_before: u64,
    pub size_after: u64,
}

impl InPlaceResult {
    /// JSON result object (key order is part of the output contract).
    pub fn to_json(&self) -> Value {
        json!({
            "applied": self.applied,
            "dry_run": self.dry_run,
            "nothing_to_trim": self.nothing_to_trim,
            "num_tools_trimmed": self.stats.num_tools_trimmed,
            "num_assistant_trimmed": self.stats.num_assistant_trimmed,
            "chars_saved": self.stats.chars_saved,
            "tokens_saved": self.stats.tokens_saved(),
            "backup_file": self.backup_file.as_ref().map(|p| p.display().to_string()),
            "session_file": self.session_file.display().to_string(),
            "size_before": self.size_before,
            "size_after": self.size_after,
        })
    }
}

pub fn run(global: &GlobalArgs, args: &TrimInPlaceArgs) -> anyhow::Result<ExitCode> {
    match run_inner(global, args) {
        Err(e) if args.json => {
            println!("{}", pyjson::dumps(&json!({ "error": format!("{e:#}") })));
            Ok(ExitCode::from(1))
        }
        other => other,
    }
}

fn run_inner(global: &GlobalArgs, args: &TrimInPlaceArgs) -> anyhow::Result<ExitCode> {
    let session_file = resolve_session(global, args)?;
    let opts = TrimOptions {
        target_tools: tool_filter(args.tools.as_deref()),
        threshold: usize::try_from(args.threshold)?,
        trim_assistant: args.trim_assistant,
    };
    let r = trim_session_in_place(&session_file, &opts, args.dry_run)?;
    if args.json {
        println!("{}", pyjson::dumps(&r.to_json()));
        return Ok(ExitCode::SUCCESS);
    }
    let verb = if r.applied { "Saved" } else { "Would save" };
    println!("Session: {}", r.session_file.display());
    println!(
        "{verb} ~{} tokens ({} chars); {} tool results and {} assistant messages trimmed",
        group(r.stats.tokens_saved()),
        group(r.stats.chars_saved),
        r.stats.num_tools_trimmed,
        r.stats.num_assistant_trimmed
    );
    if r.nothing_to_trim {
        println!("Savings too small - session left unchanged.");
    } else if let (true, Some(bak)) = (r.applied, &r.backup_file) {
        println!("Backup: {}", bak.display());
        println!(
            "File size: {} -> {} bytes",
            thousands(r.size_before),
            thousands(r.size_after)
        );
        println!("Savings take effect the next time this session is resumed.");
    }
    Ok(ExitCode::SUCCESS)
}

/// `--tools` for in-place trim: `-t ""` means all tools; empty names are dropped.
pub(super) fn tool_filter(tools: Option<&str>) -> Option<BTreeSet<String>> {
    tools.filter(|t| !t.is_empty()).map(|t| {
        let mut set = parse_tools(t);
        set.remove("");
        set
    })
}

/// Existing file path as is; else Claude-only resolution (plus exact
/// transcript stem). `--json` never prompts.
fn resolve_session(global: &GlobalArgs, args: &TrimInPlaceArgs) -> anyhow::Result<PathBuf> {
    let direct = expand_tilde(Path::new(&args.session));
    if direct.is_file() {
        return Ok(session::absolute(&direct));
    }
    let homes = global.homes()?;
    if args.json {
        return match resolve_query(&args.session, Some(Agent::Claude), &homes) {
            Ok(r) => Ok(r.session_file),
            Err(e) => exact_claude_filename(&args.session, &homes.claude).ok_or(anyhow!(e)),
        };
    }
    let opts = ResolveOptions::new(&homes)
        .agent(Some(Agent::Claude))
        .allow_claude_stem(true)
        .prompt("Which Claude session do you want to trim?");
    Ok(resolve(&args.session, &opts)?.session_file)
}

/// Trim a session file in place. The symlink-resolved file is trimmed;
/// dry runs touch nothing.
pub fn trim_session_in_place(
    path: &Path,
    opts: &TrimOptions,
    dry_run: bool,
) -> anyhow::Result<InPlaceResult> {
    if opts.threshold < 1 {
        bail!(
            "threshold must be a positive integer (got {})",
            opts.threshold
        );
    }
    if !path.is_file() {
        bail!("Session file not found: {}", path.display());
    }
    let path = fs::canonicalize(path)?;
    if let Some(a) = crate::session::meta::detect_agent_from_content(&path) {
        a.require_rewritable("trim-in-place")?;
    }
    match detect_agent_strict(&path)? {
        Some(Agent::Claude) => {}
        other => bail!(
            "In-place trim only supports Claude sessions (detected: {})",
            other.map_or("unknown", Agent::as_str)
        ),
    }
    for _ in 0..MAX_ATTEMPTS {
        if let Some(r) = trim_once(&path, opts, dry_run)? {
            return Ok(r);
        }
    }
    bail!(
        "Could not trim {}: session file changed during trim ({MAX_ATTEMPTS} attempts)",
        path.display()
    )
}

/// Read exactly the first `len` bytes of `f` (from the start).
fn read_prefix(f: &mut File, len: u64) -> anyhow::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    Read::take(&*f, len).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Files removed on drop unless released (reserved backup, temp file).
struct Scratch(Vec<PathBuf>);

impl Drop for Scratch {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = fs::remove_file(p);
        }
    }
}

/// One attempt; `None` when the file changed and the caller should retry.
fn trim_once(
    path: &Path,
    opts: &TrimOptions,
    dry_run: bool,
) -> anyhow::Result<Option<InPlaceResult>> {
    let mut orig = File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let sig = FileSnapshot::from(&orig.metadata()?);
    let mut scratch = Scratch(Vec::new());
    let backup = if dry_run {
        predict_backup(path)
    } else {
        let b = reserve_backup(path)?;
        scratch.0.push(b.clone());
        b
    };
    let bytes = read_prefix(&mut orig, sig.size)?;
    let mut t = Transcript::from_bytes(path, Agent::Claude, &bytes);
    let stats = trim_lines(
        Agent::Claude,
        &mut t.lines,
        opts,
        &backup.display().to_string(),
        None,
    );
    let out = t.to_bytes(WriteStyle::Python);
    let mut result = InPlaceResult {
        applied: false,
        dry_run,
        nothing_to_trim: stats.tokens_saved() < MIN_TOKEN_SAVINGS,
        stats,
        backup_file: None,
        session_file: path.to_path_buf(),
        size_before: sig.size,
        size_after: out.len() as u64,
    };
    if dry_run || result.nothing_to_trim {
        return Ok(Some(result));
    }
    let tmp = write_temp(path, &out, true)?;
    scratch.0.push(tmp.clone());
    let current = FileSnapshot::of(path)?;
    if (current.dev, current.ino) != (sig.dev, sig.ino) {
        return Ok(None);
    }
    copy2(path, &backup).with_context(|| format!("writing {}", backup.display()))?;
    let live = fs::metadata(path)?;
    if FileSnapshot::from(&live) != sig {
        return Ok(None);
    }
    fs::set_permissions(&tmp, live.permissions())?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    scratch.0.clear();
    merge_tail_appends(&mut orig, sig.size, out.len() as u64, path, &backup)?;
    result.size_after = fs::metadata(path)?.len();
    result.applied = true;
    result.backup_file = Some(backup);
    Ok(Some(result))
}

fn backup_candidate(path: &Path, base: &str, counter: usize) -> PathBuf {
    let suffix = if counter == 1 {
        String::new()
    } else {
        format!("-{counter}")
    };
    path.with_file_name(format!("{base}{suffix}.jsonl.bak"))
}

fn backup_base(path: &Path) -> String {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    format!(
        "{stem}.pre-trim-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    )
}

/// Backup name a real apply would likely use (nothing is created).
fn predict_backup(path: &Path) -> PathBuf {
    let base = backup_base(path);
    (1..)
        .map(|n| backup_candidate(path, &base, n))
        .find(|c| !c.exists())
        .unwrap_or_else(|| backup_candidate(path, &base, 1))
}

/// Atomically create (reserve, mode 0600) a unique backup path.
pub(super) fn reserve_backup(path: &Path) -> anyhow::Result<PathBuf> {
    let base = backup_base(path);
    for n in 1.. {
        let candidate = backup_candidate(path, &base, n);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("creating {}", candidate.display())),
        }
    }
    unreachable!("unbounded counter")
}

/// `.<stem>.trim-tmp-<rand>` next to `path`, holding `bytes` (fsynced
/// when `sync`).
fn write_temp(path: &Path, bytes: &[u8], sync: bool) -> anyhow::Result<PathBuf> {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!(
        ".{stem}.trim-tmp-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    let written = f
        .write_all(bytes)
        .and_then(|()| if sync { f.sync_all() } else { Ok(()) });
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing {}", tmp.display()));
    }
    Ok(tmp)
}

/// Bytes appended to the OLD inode after the snapshot (by a writer that
/// opened it before the swap) are spliced into the live file right after
/// the trimmed content and appended to the backup.
/// Once a chunk could only be appended, later chunks are appended too: the
/// splice offset no longer marks a line boundary (splicing at the stale
/// offset could tear lines).
fn merge_tail_appends(
    orig: &mut File,
    done: u64,
    insert_at: u64,
    path: &Path,
    backup: &Path,
) -> anyhow::Result<()> {
    let mut insert_at = Some(insert_at);
    drain_growth(orig, done, &mut |chunk| {
        insert_at = match insert_at {
            Some(at) => match splice(path, at, chunk)? {
                Placed::Inserted => Some(at + chunk.len() as u64),
                Placed::Appended => None,
            },
            None => {
                append_bytes(path, chunk)?;
                None
            }
        };
        append_bytes(backup, chunk)
    })
    .map(drop)
}

/// Where [`splice`] put the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placed {
    /// At the requested offset.
    Inserted,
    /// At the end (every attempt lost the race).
    Appended,
}

/// Feed bytes appended to `f` past `done` to `sink` until `f` has been
/// quiet for 250 ms (max 5 s). Returns the bytes handled in total.
fn drain_growth(
    f: &mut File,
    mut done: u64,
    sink: &mut dyn FnMut(&[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<u64> {
    let deadline = Instant::now() + MERGE_MAX;
    let mut quiet_until = Instant::now() + MERGE_QUIET;
    loop {
        let size = f.metadata()?.len();
        if size > done {
            f.seek(SeekFrom::Start(done))?;
            let mut chunk = Vec::new();
            Read::take(&*f, size - done).read_to_end(&mut chunk)?;
            if !chunk.is_empty() {
                sink(&chunk)?;
                done += chunk.len() as u64;
                quiet_until = Instant::now() + MERGE_QUIET;
                continue;
            }
        }
        let now = Instant::now();
        if now >= quiet_until || now >= deadline {
            return Ok(done);
        }
        std::thread::sleep(MERGE_POLL);
    }
}

/// Append `data` with `O_APPEND` (one `write_all`).
fn append_bytes(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(data))
        .with_context(|| format!("appending to {}", path.display()))
}

/// Insert `data` into `path` at offset `at`:
/// rebuild head + data + tail in a temp file, re-check the live file's
/// signature right before the rename (retrying on change), then salvage
/// anything appended to the replaced inode after it was read. When every
/// attempt loses the race, `data` is appended instead: bytes are never lost.
fn splice(path: &Path, at: u64, data: &[u8]) -> anyhow::Result<Placed> {
    for _ in 0..MAX_ATTEMPTS {
        let mut live = File::open(path).with_context(|| format!("reading {}", path.display()))?;
        let md = live.metadata()?;
        let sig = FileSnapshot::from(&md);
        let bytes = read_prefix(&mut live, sig.size)?;
        let at = usize::try_from(at)?.min(bytes.len());
        let mut out = Vec::with_capacity(bytes.len() + data.len());
        out.extend_from_slice(&bytes[..at]);
        out.extend_from_slice(data);
        out.extend_from_slice(&bytes[at..]);
        // No fsync: the window between the read and the rename must stay short.
        let tmp = write_temp(path, &out, false)?;
        let mut scratch = Scratch(vec![tmp.clone()]);
        fs::set_permissions(&tmp, md.permissions())?;
        if FileSnapshot::of(path)? != sig {
            continue;
        }
        fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        scratch.0.clear();
        // Writes that landed on the replaced inode after it was read.
        drain_growth(&mut live, sig.size, &mut |chunk| append_bytes(path, chunk))?;
        return Ok(Placed::Inserted);
    }
    append_bytes(path, data)?;
    Ok(Placed::Appended)
}
