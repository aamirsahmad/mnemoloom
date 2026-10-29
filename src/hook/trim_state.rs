//! Private, atomically written pending plans. Disk JSON is untrusted.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Number, Value};

use super::trim_args::Options;
use crate::session::paths::cache_root;
pub(super) use crate::session::timefmt::now;
use crate::session::transcript::write_atomic_with;

pub(super) const PENDING_TTL_SECS: f64 = 600.0;
const FUTURE_SLACK_SECS: f64 = 60.0;
const MAX_TRUSTED_NUMBER: f64 = 1_000_000_000_000_000.0;
const MAX_PLAN_BYTES: u64 = 1024 * 1024;

// O_NOFOLLOW | O_NONBLOCK: refuse symlinks and never block on a planted FIFO.
const SAFE_OPEN_FLAGS: i32 = libc::O_NOFOLLOW | libc::O_NONBLOCK;

fn current_uid() -> u32 {
    // POSIX getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

pub(super) fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct Plan {
    pub created_at: f64,
    pub transcript_path: String,
    pub opts: Options,
    pub preview_tokens: Option<Number>,
}

fn finite_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|n| n.is_finite() && n.abs() <= MAX_TRUSTED_NUMBER)
}

impl Plan {
    pub fn from_value(value: Value) -> Option<Self> {
        let object = value.as_object()?;
        let created_at = finite_number(object.get("created_at")?)?;
        let transcript_path = object.get("transcript_path")?.as_str()?;
        if transcript_path.is_empty() {
            return None;
        }
        let opts: Options = serde_json::from_value(object.get("opts")?.clone()).ok()?;
        if !opts.is_valid() {
            return None;
        }
        let preview_tokens = match object.get("preview_tokens") {
            None | Some(Value::Null) => None,
            Some(n) => {
                finite_number(n)?;
                Some(n.as_number()?.clone())
            }
        };
        Some(Self {
            created_at,
            transcript_path: transcript_path.to_owned(),
            opts,
            preview_tokens,
        })
    }
}

#[derive(Clone, Debug)]
pub(super) struct State {
    dir: Option<PathBuf>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            dir: Some(cache_root().join("hook")),
        }
    }
}

impl State {
    #[cfg(test)]
    pub fn from_cache_dir(cache: Option<PathBuf>) -> Self {
        Self {
            dir: cache.map(|p| p.join("mnemoloom").join("hook")),
        }
    }

    fn path(&self, session_id: &str) -> Option<PathBuf> {
        is_safe_session_id(session_id).then_some(())?;
        Some(
            self.dir
                .as_ref()?
                .join(format!("trim-pending.{session_id}.json")),
        )
    }

    fn secure_dir(&self, create: bool) -> Result<&Path, String> {
        let dir = self
            .dir
            .as_deref()
            .ok_or("Cannot determine a private trim state cache directory.")?;
        // The cache root is user configuration shared with the search index
        // and `mux` (it may be a symlink, and its mode is the user's): only
        // make sure it exists. The private-directory rules apply to our own
        // `hook` subdirectory.
        if create && let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot use trim state dir {}: {e}", parent.display()))?;
        }
        secure_directory(dir, create, current_uid())?;
        Ok(dir)
    }

    pub fn save(&self, session_id: &str, plan: &Plan) -> Result<(), String> {
        if !is_safe_session_id(session_id) {
            return Err("Session ID looks invalid; not touching trim state.".to_owned());
        }
        let dir = self.secure_dir(true)?;
        let path = dir.join(format!("trim-pending.{session_id}.json"));
        let written = (|| -> anyhow::Result<()> {
            let bytes = serde_json::to_vec(plan)?;
            write_atomic_with(&path, &bytes, None, Some(0o600), &|| Ok(()))
        })();
        written
            .map_err(|error| format!("Cannot write trim state under {}: {error:#}", dir.display()))
    }

    pub fn load(&self, session_id: &str, now: f64) -> Option<Plan> {
        let path = self.path(session_id)?;
        self.secure_dir(false).ok()?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(SAFE_OPEN_FLAGS)
            .open(&path)
            .ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file()
            || metadata.uid() != current_uid()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return None;
        }
        if metadata.len() > MAX_PLAN_BYTES {
            let _ = fs::remove_file(path);
            return None;
        }
        let value: Value = serde_json::from_reader(Read::take(file, MAX_PLAN_BYTES + 1)).ok()?;
        let plan = Plan::from_value(value).filter(|plan| {
            plan.created_at <= now + FUTURE_SLACK_SECS && plan.created_at >= now - PENDING_TTL_SECS
        });
        if plan.is_none() {
            let _ = fs::remove_file(path);
        }
        plan
    }

    pub fn clear(&self, session_id: &str) {
        if let Some(path) = self.path(session_id)
            && self.secure_dir(false).is_ok()
        {
            let _ = fs::remove_file(path);
        }
    }
}

fn secure_directory(path: &Path, create: bool, uid: u32) -> Result<(), String> {
    let inspect = || -> std::io::Result<fs::Metadata> {
        if create {
            DirBuilder::new().recursive(true).mode(0o700).create(path)?;
        }
        fs::symlink_metadata(path)
    };
    let metadata =
        inspect().map_err(|e| format!("Cannot use trim state dir {}: {e}", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!(
            "Trim state dir {} is not a real directory (symlink or file) - refusing to touch trim state there.",
            path.display()
        ));
    }
    if metadata.uid() != uid {
        return Err(format!(
            "Trim state dir {} is owned by another user - refusing to touch trim state there.",
            path.display()
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        // Open and verify the same inode before chmod, rather than following
        // a directory symlink swapped into place after lstat.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(SAFE_OPEN_FLAGS)
            .open(path)
            .map_err(|e| format!("Cannot make trim state dir {} private: {e}", path.display()))?;
        let opened = file
            .metadata()
            .map_err(|e| format!("Cannot make trim state dir {} private: {e}", path.display()))?;
        if !same_directory(&metadata, &opened) {
            return Err(format!(
                "Cannot make trim state dir {} private: directory changed during validation",
                path.display()
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Cannot make trim state dir {} private: {e}", path.display()))?;
    }
    Ok(())
}

fn same_directory(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    after.is_dir()
        && before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.uid() == after.uid()
}

#[cfg(test)]
#[path = "trim_state_tests.rs"]
mod tests;
