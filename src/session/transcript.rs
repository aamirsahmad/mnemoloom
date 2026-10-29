//! Whole-transcript access with raw bytes preserved for faithful rewriting.
//!
//! Every physical line keeps its original bytes. A line is re-serialized on
//! write only after it was changed through [`Line::set_value`] /
//! [`Line::value_mut`]; everything else (blank, unparseable, invalid UTF-8,
//! untouched) is written back byte-for-byte.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::Value;

use super::message::{Message, MessageKind, parse_line};
use super::{Agent, pyjson};

/// How modified lines are serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteStyle {
    /// serde_json compact (`{"a":1}`), as Claude Code / Codex write.
    #[default]
    Compact,
    /// Python-style JSON (`{"a": 1}`, ASCII-escaped); see [`pyjson`].
    Python,
}

/// One physical JSONL line.
#[derive(Debug, Clone)]
pub struct Line {
    raw: Vec<u8>,
    value: Option<Value>,
    dirty: bool,
}

impl Line {
    /// Build from raw bytes (no trailing newline); parses JSON if possible.
    pub fn from_raw(raw: Vec<u8>) -> Line {
        let value = if raw.iter().all(u8::is_ascii_whitespace) {
            None
        } else {
            serde_json::from_slice(&raw).ok()
        };
        Line {
            raw,
            value,
            dirty: false,
        }
    }

    /// A new line holding `value` (serialized on write).
    pub fn from_value(value: Value) -> Line {
        Line {
            raw: Vec::new(),
            value: Some(value),
            dirty: true,
        }
    }

    /// Original bytes (without the newline).
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// Parsed JSON, if the line parsed.
    pub fn value(&self) -> Option<&Value> {
        self.value.as_ref()
    }

    /// Mutable parsed JSON; marks the line modified.
    pub fn value_mut(&mut self) -> Option<&mut Value> {
        self.dirty = self.value.is_some();
        self.value.as_mut()
    }

    /// Replace the value; the line will be re-serialized on write.
    pub fn set_value(&mut self, value: Value) {
        self.value = Some(value);
        self.dirty = true;
    }

    /// Whether the line will be re-serialized.
    pub fn is_modified(&self) -> bool {
        self.dirty
    }

    /// Bytes that will be written (without newline).
    pub fn output_bytes(&self, style: WriteStyle) -> Vec<u8> {
        match (&self.value, self.dirty) {
            (Some(v), true) => match style {
                WriteStyle::Compact => serde_json::to_vec(v).unwrap_or_else(|_| self.raw.clone()),
                WriteStyle::Python => pyjson::dumps(v).into_bytes(),
            },
            _ => self.raw.clone(),
        }
    }
}

/// A fully loaded session file.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub path: PathBuf,
    pub agent: Agent,
    pub lines: Vec<Line>,
    /// Whether the source ended with `\n` (preserved on write).
    pub trailing_newline: bool,
}

impl Transcript {
    /// Read and parse every line of `path`. An opencode session (virtual
    /// path) loads one synthetic line per message from its DB
    /// ([`super::opencode::transcript_values`]); such a transcript is
    /// read-only (never write it back).
    pub fn load(path: &Path, agent: Agent) -> anyhow::Result<Transcript> {
        if agent == Agent::OpenCode {
            return Self::load_opencode(path);
        }
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(Self::from_bytes(path, agent, &bytes))
    }

    fn load_opencode(path: &Path) -> anyhow::Result<Transcript> {
        let (db, id) = super::opencode::split_virtual(path)
            .with_context(|| format!("not an opencode session: {}", path.display()))?;
        anyhow::ensure!(
            super::opencode::exists(path),
            "no opencode session at {}",
            path.display()
        );
        let values = super::opencode::transcript_values(&db, &id)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(Transcript {
            path: path.to_path_buf(),
            agent: Agent::OpenCode,
            lines: values
                .into_iter()
                .map(|v| Line {
                    raw: serde_json::to_vec(&v).unwrap_or_default(),
                    value: Some(v),
                    dirty: false,
                })
                .collect(),
            trailing_newline: true,
        })
    }

    /// Parse transcript bytes (no I/O).
    pub fn from_bytes(path: &Path, agent: Agent, bytes: &[u8]) -> Transcript {
        let trailing_newline = bytes.last() == Some(&b'\n');
        let body = if trailing_newline {
            &bytes[..bytes.len() - 1]
        } else {
            bytes
        };
        let lines = if bytes.is_empty() {
            Vec::new()
        } else {
            body.split(|&b| b == b'\n')
                .map(|l| Line::from_raw(l.to_vec()))
                .collect()
        };
        Transcript {
            path: path.to_path_buf(),
            agent,
            lines,
            trailing_newline,
        }
    }

    /// All messages in file order; tool results get `tool_name` filled from
    /// the matching tool use (`"Unknown"` is NOT substituted). For pi this
    /// includes abandoned branches (see [`Transcript::display_messages`]).
    pub fn messages(&self) -> Vec<Message> {
        self.messages_where(|_| true)
    }

    /// The conversation as the agent shows it: [`Transcript::messages`],
    /// restricted for pi to the active branch ([`super::pi::active_branch`]).
    pub fn display_messages(&self) -> Vec<Message> {
        if self.agent != Agent::Pi {
            return self.messages();
        }
        let keep = super::pi::active_branch(self.lines.iter().map(Line::value));
        self.messages_where(|i| keep.contains(&i))
    }

    fn messages_where(&self, keep: impl Fn(usize) -> bool) -> Vec<Message> {
        let mut out: Vec<Message> = self
            .lines
            .iter()
            .enumerate()
            .filter(|(i, _)| keep(*i))
            .filter_map(|(i, l)| l.value().map(|v| parse_line(self.agent, i, v)))
            .flatten()
            .collect();
        let names: HashMap<String, String> = out
            .iter()
            .filter(|m| m.kind == MessageKind::ToolUse)
            .filter_map(|m| Some((m.tool_id.clone()?, m.tool_name.clone()?)))
            .collect();
        for m in out.iter_mut().filter(|m| m.kind == MessageKind::ToolResult) {
            if let Some(id) = &m.tool_id {
                m.tool_name = names.get(id).cloned();
            }
        }
        out
    }

    /// Serialized file contents.
    pub fn to_bytes(&self, style: WriteStyle) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, l) in self.lines.iter().enumerate() {
            if i > 0 {
                out.push(b'\n');
            }
            out.extend_from_slice(&l.output_bytes(style));
        }
        if self.trailing_newline && !self.lines.is_empty() {
            out.push(b'\n');
        }
        out
    }

    /// Atomically write to `dest` (temp file in the same dir + rename).
    /// The file mode is taken from an existing `dest`, else `mode_source`.
    pub fn write_atomic(
        &self,
        dest: &Path,
        style: WriteStyle,
        mode_source: Option<&Path>,
    ) -> anyhow::Result<()> {
        write_atomic(dest, &self.to_bytes(style), mode_source)
    }
}

/// Atomically replace `dest` with `bytes`: temp file `.<name>.<uuid>.mnemoloom-tmp`
/// ([`temp_sibling`]) next to it, fsync, rename. Mode preserved from `dest`
/// or `mode_source`.
pub fn write_atomic(dest: &Path, bytes: &[u8], mode_source: Option<&Path>) -> anyhow::Result<()> {
    write_atomic_with(dest, bytes, mode_source, None, &|| Ok(()))
}

/// [`write_atomic`] that runs `before_rename` after the temp file is
/// durable and immediately before the rename; an error there aborts the
/// write (temp removed, `dest` untouched). Use it to re-check that a
/// source didn't change ([`FileSnapshot::ensure_unchanged`]).
/// An explicit creation `mode` (subject to umask) skips permission inheritance
/// and never stats `dest`, which may be an untrusted symlink.
pub fn write_atomic_with(
    dest: &Path,
    bytes: &[u8],
    mode_source: Option<&Path>,
    mode: Option<u32>,
    before_rename: &dyn Fn() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let inherit = mode.is_none();
    let mode = mode.or_else(|| {
        [Some(dest), mode_source]
            .into_iter()
            .flatten()
            .find_map(|p| fs::metadata(p).ok().filter(|m| m.is_file()))
            .map(|m| m.permissions().mode() & 0o7777)
    });
    let tmp = temp_sibling(dest);
    let result = (|| -> anyhow::Result<()> {
        let mut f: File = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode.unwrap_or(0o666))
            .open(&tmp)?;
        if inherit && let Some(mode) = mode {
            f.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        f.write_all(bytes)?;
        f.sync_all()?;
        before_rename()?;
        fs::rename(&tmp, dest)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| format!("writing {}", dest.display()))
}

/// A unique hidden temp path next to `dest`: `.<name>.<uuid>.mnemoloom-tmp`.
pub fn temp_sibling(dest: &Path) -> std::path::PathBuf {
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    dir.join(format!(
        ".{name}.{}.mnemoloom-tmp",
        uuid::Uuid::new_v4().simple()
    ))
}

/// Identity + size + mtime of a file, to detect concurrent appends
/// (e.g. a live agent still writing the session).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSnapshot {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i128,
}

impl From<&fs::Metadata> for FileSnapshot {
    fn from(m: &fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime_ns: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()),
        }
    }
}

impl FileSnapshot {
    /// Stat `path` (following symlinks).
    pub fn of(path: &Path) -> std::io::Result<FileSnapshot> {
        Ok(Self::from(&fs::metadata(path)?))
    }

    /// Error if `path` no longer matches this snapshot.
    pub fn ensure_unchanged(&self, path: &Path) -> anyhow::Result<()> {
        let now =
            FileSnapshot::of(path).with_context(|| format!("re-checking {}", path.display()))?;
        if now != *self {
            anyhow::bail!(
                "session file changed while it was being processed (is the session still running?): {}",
                path.display()
            );
        }
        Ok(())
    }
}

impl Transcript {
    /// [`Transcript::load`] plus a [`FileSnapshot`] taken before reading;
    /// fails if the file grew while it was read.
    pub fn load_with_snapshot(
        path: &Path,
        agent: Agent,
    ) -> anyhow::Result<(Transcript, FileSnapshot)> {
        let snap = FileSnapshot::of(path).with_context(|| format!("reading {}", path.display()))?;
        let t = Transcript::load(path, agent)?;
        snap.ensure_unchanged(path)?;
        Ok((t, snap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testutil::*;
    use serde_json::json;

    #[test]
    fn pi_display_messages_follow_the_active_branch() {
        let d = tempfile::tempdir().unwrap();
        let p = write_pi(d.path(), PI_ID, "/w", &pi_lines(PI_ID, "/w"));
        let t = Transcript::load(&p, Agent::Pi).unwrap();
        let texts = |ms: Vec<Message>| -> Vec<String> {
            ms.into_iter()
                .filter(|m| m.role == super::super::Role::User)
                .map(|m| m.text)
                .collect()
        };
        assert_eq!(
            texts(t.messages()),
            ["add a rate limiter", "try redis", "use a token bucket"]
        );
        assert_eq!(
            texts(t.display_messages()),
            ["add a rate limiter", "use a token bucket"]
        );
        let result = t
            .display_messages()
            .into_iter()
            .find(|m| m.kind == MessageKind::ToolResult)
            .unwrap();
        assert_eq!(result.tool_name.as_deref(), Some("bash"));
    }

    #[test]
    fn opencode_loads_from_the_database() {
        let d = tempfile::tempdir().unwrap();
        let db = write_opencode_db(&d.path().join("opencode.db"), "/w", 1_000);
        let path = super::super::opencode::virtual_path(&db, OPENCODE_ID);
        let t = Transcript::load(&path, Agent::OpenCode).unwrap();
        assert_eq!(t.lines.len(), 2);
        assert!(t.lines.iter().all(|l| !l.is_modified()));
        let ms = t.display_messages();
        assert_eq!(
            ms.first().map(|m| m.text.as_str()),
            Some("add a rate limiter")
        );
        assert_eq!(ms.last().map(|m| m.text.as_str()), Some("Done."));
        let missing = super::super::opencode::virtual_path(&db, "ses_missing");
        assert!(Transcript::load(&missing, Agent::OpenCode).is_err());
        assert!(Transcript::load(Path::new("/nope.jsonl"), Agent::OpenCode).is_err());
    }

    #[test]
    fn roundtrip_preserves_bytes() {
        let src = b"{\"b\":1,\"a\":2}\n\nnot json\n{\"x\": \"\xff\"}\n{\"type\":\"user\"}\n";
        let t = Transcript::from_bytes(Path::new("/x.jsonl"), Agent::Claude, src);
        assert_eq!(t.lines.len(), 5);
        assert!(t.lines[2].value().is_none());
        assert!(t.lines[3].value().is_none(), "invalid UTF-8 is not parsed");
        assert_eq!(t.to_bytes(WriteStyle::Compact), src);
    }

    #[test]
    fn modified_lines_reserialize_in_order() {
        let src = b"{\"b\":1,\"a\":2}\n{\"c\":3}";
        let mut t = Transcript::from_bytes(Path::new("/x.jsonl"), Agent::Claude, src);
        t.lines[0].value_mut().unwrap()["a"] = json!("é");
        assert!(t.lines[0].is_modified() && !t.lines[1].is_modified());
        assert_eq!(
            t.to_bytes(WriteStyle::Compact),
            "{\"b\":1,\"a\":\"é\"}\n{\"c\":3}".as_bytes()
        );
        assert_eq!(
            t.to_bytes(WriteStyle::Python),
            b"{\"b\": 1, \"a\": \"\\u00e9\"}\n{\"c\":3}".to_vec()
        );
    }

    #[test]
    fn tool_names_paired() {
        let src = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"x"}]}}
"#;
        let t = Transcript::from_bytes(Path::new("/x"), Agent::Claude, src);
        let ms = t.messages();
        assert_eq!(ms[1].tool_name.as_deref(), Some("Read"));
    }

    #[test]
    fn guarded_write_aborts_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("s.jsonl");
        fs::write(&src, "a\n").unwrap();
        assert_eq!(
            FileSnapshot::of(&src).unwrap(),
            FileSnapshot::from(&fs::File::open(&src).unwrap().metadata().unwrap())
        );
        let (_, snap) = Transcript::load_with_snapshot(&src, Agent::Claude).unwrap();
        fs::write(&src, "a\nb\n").unwrap();
        let err = write_atomic_with(&src, b"x\n", None, None, &|| snap.ensure_unchanged(&src))
            .unwrap_err();
        assert!(format!("{err:#}").contains("changed while"));
        assert_eq!(fs::read(&src).unwrap(), b"a\nb\n", "dest untouched");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "temp removed");
    }

    #[test]
    fn atomic_write_keeps_mode() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.jsonl");
        fs::write(&src, "a\n").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o600)).unwrap();
        let dest = dir.path().join("dest.jsonl");
        write_atomic(&dest, b"b\n", Some(&src)).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"b\n");
        assert_eq!(
            fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            2,
            "no temp left behind"
        );
        fs::remove_file(&dest).unwrap();
        std::os::unix::fs::symlink(&src, &dest).unwrap();
        write_atomic_with(&dest, b"private\n", None, Some(0o600), &|| Ok(())).unwrap();
        assert!(fs::symlink_metadata(&dest).unwrap().is_file());
        assert_eq!(fs::read(&src).unwrap(), b"a\n");
        assert_eq!(
            fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
