//! On-disk index location, version stamp and incremental-update state.
//!
//! Layout of the index dir: tantivy files, `VERSION` (`mnemoloom-idx<REVISION>`),
//! `.mnemoloom-build.lock` and `index_state.json`
//! (`{abs path: {mtime_ns, size[, excluded_cwd]}}`).

use std::collections::HashMap;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tantivy::Index;
use tantivy::directory::MmapDirectory;

use super::schema::{Fields, build_schema};
use crate::session::homes::expand_tilde;
use crate::session::paths::cache_root;
use crate::session::transcript::write_atomic;

/// Bump whenever extraction, keys or the schema change: a mismatch wipes
/// and rebuilds the index.
pub const INDEX_REVISION: u32 = 3;
const VERSION_FILE: &str = "VERSION";
const LOCK_FILE: &str = ".mnemoloom-build.lock";
const STATE_FILE: &str = "index_state.json";
/// Env override for the index directory.
pub const INDEX_DIR_ENV: &str = "MNEMOLOOM_INDEX_DIR";

/// `mnemoloom-idx<REVISION>` (independent of the crate version).
pub fn version_stamp() -> String {
    format!("mnemoloom-idx{INDEX_REVISION}")
}

/// `$MNEMOLOOM_INDEX_DIR` > `<cache root>/search-index` (see [`cache_root`]).
pub fn default_index_dir() -> PathBuf {
    match std::env::var_os(INDEX_DIR_ENV).filter(|v| !v.is_empty()) {
        Some(d) => expand_tilde(Path::new(&d)),
        None => cache_root().join("search-index"),
    }
}

/// `-i PATH` (tilde-expanded) or the default.
pub fn index_dir_arg(arg: Option<&Path>) -> PathBuf {
    arg.map(expand_tilde).unwrap_or_else(default_index_dir)
}

/// Last-indexed stat of one session file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub mtime_ns: u64,
    pub size: u64,
}

/// One `index_state.json` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileState {
    #[serde(flatten)]
    pub stamp: Stamp,
    /// Set when the file was skipped because its cwd lies inside an agent
    /// home; re-checked against the current homes on every run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_cwd: Option<String>,
}

pub type State = HashMap<String, FileState>;

/// Load the state map (empty when missing or unreadable).
pub fn load_state(dir: &Path) -> State {
    fs::read(dir.join(STATE_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Atomically write the state map.
pub fn save_state(dir: &Path, state: &State) -> anyhow::Result<()> {
    write_atomic(&dir.join(STATE_FILE), &serde_json::to_vec(state)?, None)
}

/// Whether a tantivy index exists in `dir`.
pub fn index_exists(dir: &Path) -> bool {
    dir.join("meta.json").is_file()
}

/// Exclusive advisory lock on `<dir>/.mnemoloom-build.lock`, held across the
/// version check, wipe, state load, indexing, commit and state save.
#[derive(Debug)]
pub struct BuildLock(#[allow(dead_code)] File);

/// Whether to wait for a lock held by another process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockWait {
    Block,
    Try,
}

/// True when `dir` holds an mnemoloom index (any revision) or nothing but our lock.
pub fn is_ours_or_empty(dir: &Path) -> bool {
    if fs::read_to_string(dir.join(VERSION_FILE)).is_ok_and(|v| v.starts_with("mnemoloom-")) {
        return true;
    }
    fs::read_dir(dir).is_ok_and(|rd| rd.flatten().all(|e| e.file_name() == LOCK_FILE))
}

fn not_ours(dir: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "Refusing to use {}: it is not an mnemoloom search index (no mnemoloom VERSION file).",
        dir.display()
    )
}

/// Take the build lock (creating `dir`). `None` when `Try` and busy.
/// Refuses dirs that are not mnemoloom indexes (checked under the lock).
pub fn lock(dir: &Path, wait: LockWait) -> anyhow::Result<Option<BuildLock>> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(LOCK_FILE);
    let existed = path.exists();
    // Only check ownership under the lock: unlocked, a concurrent builder's
    // in-flight VERSION temp file makes a fresh dir look foreign.
    let f = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .context("opening index lock")?;
    match wait {
        LockWait::Block => f.lock().context("locking index")?,
        LockWait::Try => match f.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(e).context("locking index"),
        },
    }
    if !is_ours_or_empty(dir) {
        if !existed {
            let _ = fs::remove_file(&path);
        }
        return Err(not_ours(dir));
    }
    Ok(Some(BuildLock(f)))
}

/// Under the lock: when the stamp differs (or is missing on a non-empty
/// dir), wipe everything but the lock and write the current stamp first.
/// Returns the notice to show, if any.
pub fn check_version(dir: &Path, _lock: &BuildLock) -> anyhow::Result<Option<String>> {
    let want = version_stamp();
    let have = fs::read_to_string(dir.join(VERSION_FILE)).ok();
    if have.as_deref().map(str::trim) == Some(want.as_str()) {
        return Ok(None);
    }
    let note = have.is_some().then(|| {
        format!(
            "Index format changed ({} -> {want}), rebuilding index...",
            have.as_deref().unwrap_or_default().trim()
        )
    });
    for e in fs::read_dir(dir)?.flatten() {
        if e.file_name() == LOCK_FILE {
            continue;
        }
        let p = e.path();
        if e.file_type()?.is_dir() {
            fs::remove_dir_all(&p)?;
        } else {
            fs::remove_file(&p)?;
        }
    }
    write_atomic(&dir.join(VERSION_FILE), want.as_bytes(), None)
        .context("writing index VERSION")?;
    Ok(note)
}

/// Remove an index dir; refuses dirs that are not mnemoloom indexes.
pub fn clear(dir: &Path) -> anyhow::Result<()> {
    if !is_ours_or_empty(dir) {
        return Err(not_ours(dir));
    }
    let _lock = lock(dir, LockWait::Block)?;
    fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))
}

/// Open the index in `dir` (which exists), creating the index when missing.
pub fn open_or_create(dir: &Path, _lock: &BuildLock) -> anyhow::Result<(Index, Fields)> {
    let mmap = MmapDirectory::open(dir)?;
    let index = Index::open_or_create(mmap, build_schema())?;
    let fields = Fields::new(&index.schema())?;
    Ok((index, fields))
}

/// Open an existing index read-only (`None` when there is none).
pub fn open_existing(dir: &Path) -> anyhow::Result<Option<(Index, Fields)>> {
    if !index_exists(dir) {
        return Ok(None);
    }
    let index = Index::open_in_dir(dir)
        .context("Failed to open index. Run 'mnemoloom build-index' first.")?;
    let fields = Fields::new(&index.schema())?;
    Ok(Some((index, fields)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_mismatch_wipes_but_keeps_lock() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("idx");
        let l = lock(&d, LockWait::Block).unwrap().unwrap();
        assert_eq!(check_version(&d, &l).unwrap(), None);
        assert_eq!(
            fs::read_to_string(d.join(VERSION_FILE)).unwrap(),
            version_stamp()
        );
        fs::write(d.join(VERSION_FILE), "mnemoloom-0.1.0+idx1").unwrap();
        fs::write(d.join(STATE_FILE), "{}").unwrap();
        let note = check_version(&d, &l).unwrap().unwrap();
        assert_eq!(
            note,
            format!(
                "Index format changed (mnemoloom-0.1.0+idx1 -> {}), rebuilding index...",
                version_stamp()
            )
        );
        assert!(!d.join(STATE_FILE).exists() && d.join(LOCK_FILE).exists());
        assert_eq!(check_version(&d, &l).unwrap(), None);
    }

    #[test]
    fn lock_excludes_and_foreign_dirs_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("idx");
        let held = lock(&d, LockWait::Block).unwrap();
        assert!(held.is_some());
        assert!(lock(&d, LockWait::Try).unwrap().is_none());
        drop(held);
        // Block, not Try: a child that a parallel test is spawning can hold a
        // copy of the lock fd until it execs.
        assert!(lock(&d, LockWait::Block).unwrap().is_some());
        let foreign = dir.path().join("foreign");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("meta.json"), "{}").unwrap();
        assert!(lock(&foreign, LockWait::Block).is_err());
        assert!(!foreign.join(LOCK_FILE).exists());
        assert!(clear(&foreign).is_err());
        assert!(foreign.join("meta.json").exists());
        clear(&d).unwrap();
        assert!(!d.exists());
    }

    #[test]
    fn state_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = State::new();
        let stamp = Stamp {
            mtime_ns: 5,
            size: 7,
        };
        s.insert(
            "/a".into(),
            FileState {
                stamp,
                excluded_cwd: None,
            },
        );
        s.insert(
            "/b".into(),
            FileState {
                stamp,
                excluded_cwd: Some("/h/.claude".into()),
            },
        );
        save_state(dir.path(), &s).unwrap();
        assert_eq!(load_state(dir.path()), s);
        let raw = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
        assert!(raw.contains(r#""/a":{"mtime_ns":5,"size":7}"#));
    }
}
