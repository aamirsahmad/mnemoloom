//! tmux interaction: pane listing, screen capture, and jumping.

use std::borrow::Cow;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use regex::Regex;

use rayon::prelude::*;

use super::exec::{self, SLOW, tmux};

/// Field separator for `-F` output; does not occur in paths or titles.
const SEP: char = '\x1f';
/// Record terminator: titles and paths may contain newlines.
const REC: char = '\x1e';
/// Printed after each capture in a chained call. Printable on purpose: tmux
/// 3.5 escapes control characters in `display-message` output.
const CAPTURE_END: &str = "@@mux-capture-end:5f0c2e@@";
/// Lines of screen text kept for state detection.
const SCREEN_LINES: usize = 40;
/// Lines shown in the picker preview.
const PREVIEW_LINES: usize = 60;

static ANSI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)").expect("static regex")
});

/// One row of `tmux list-panes -a`.
#[derive(Debug, PartialEq)]
pub struct PaneRecord {
    pub pane: String,
    pub session: String,
    pub pid: u32,
    pub title: String,
    pub cwd: String,
    /// `#{host}` and `#{host_short}`: tmux's default pane title.
    pub hosts: [String; 2],
}

/// Whether a tmux server is reachable.
pub fn server_running() -> bool {
    !tmux(&["list-sessions"]).is_empty()
}

/// Whether a `tmux` binary is on PATH (spawning it is not `NotFound`).
pub fn installed() -> bool {
    let status = Command::new("tmux")
        .arg("-V")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    !matches!(status, Err(e) if e.kind() == io::ErrorKind::NotFound)
}

pub fn list_panes() -> Vec<PaneRecord> {
    let fmt = [
        "#{session_name}:#{window_index}.#{pane_index}",
        "#{session_name}",
        "#{pane_pid}",
        "#{pane_title}",
        "#{pane_current_path}",
        "#{host}",
        "#{host_short}",
    ]
    .join(&SEP.to_string());
    let listing = tmux(&["list-panes", "-a", "-F", &format!("{fmt}{REC}")]);
    parse_listing(&decode_listing(&listing))
}

/// tmux 3.5 (e.g. Debian) vis-encodes format output: control characters
/// come back as `\037`, a backslash as `\\`, a newline as `\n`. tmux 3.7
/// prints them raw. Raw separators mean nothing was encoded; otherwise decode
/// before splitting, so an encoded backslash can never forge a separator.
fn decode_listing(listing: &str) -> Cow<'_, str> {
    if listing.contains(REC) {
        Cow::Borrowed(listing)
    } else {
        Cow::Owned(unvis(listing))
    }
}

/// Undo `vis(3)` with `VIS_OCTAL | VIS_CSTYLE` (octal `\NNN` and C escapes).
fn unvis(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let (byte, used) = match bytes[i..] {
            [b'\\', a @ b'0'..=b'3', b @ b'0'..=b'7', c @ b'0'..=b'7', ..] => {
                ((a - b'0') << 6 | (b - b'0') << 3 | (c - b'0'), 4)
            }
            [b'\\', escape, ..] => match escape {
                b'\\' => (b'\\', 2),
                b'n' => (b'\n', 2),
                b't' => (b'\t', 2),
                b'r' => (b'\r', 2),
                b'a' => (0x07, 2),
                b'b' => (0x08, 2),
                b'f' => (0x0c, 2),
                b'v' => (0x0b, 2),
                b's' => (b' ', 2),
                b'0' => (0, 2),
                _ => (b'\\', 1),
            },
            [other, ..] => (other, 1),
            [] => break,
        };
        out.push(byte);
        i += used;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Split on the record marker. tmux adds a newline after each `-F` line, so
/// exactly one leading newline per record is separator; the rest is data.
pub fn parse_listing(listing: &str) -> Vec<PaneRecord> {
    listing
        .split(REC)
        .filter_map(|record| {
            let record = record.strip_prefix('\n').unwrap_or(record);
            let parts: Vec<&str> = record.split(SEP).collect();
            let [pane, session, pid, title, cwd, host, host_short] = parts[..] else {
                return None;
            };
            Some(PaneRecord {
                pane: pane.into(),
                session: session.into(),
                pid: pid.parse().ok()?,
                title: title.into(),
                cwd: cwd.into(),
                hosts: [host.into(), host_short.into()],
            })
        })
        .collect()
}

/// A pane's visible screen with escapes stripped, for state detection.
/// `None` when the pane is gone (tmux failed); `Some("")` for a blank screen,
/// e.g. an agent that has not drawn anything yet.
pub fn capture(pane: &str) -> Option<String> {
    exec::run("tmux", &["capture-pane", "-p", "-t", pane], &[], SLOW).map(|raw| clean_screen(&raw))
}

/// Visible screens of `panes`, in order, escapes stripped; `None` for a
/// pane that no longer exists.
///
/// The server handles captures one at a time, so a single client running a
/// chained `capture-pane ; display-message <marker>` sequence is ~10x faster
/// than one client per pane. If a pane vanished the chain aborts, and every
/// pane is then captured on its own, concurrently.
pub fn capture_all(panes: &[&str]) -> Vec<Option<String>> {
    if panes.is_empty() {
        return Vec::new();
    }
    let mut args = Vec::with_capacity(panes.len() * 9);
    for pane in panes {
        args.extend(["capture-pane", "-p", "-t", pane, ";"]);
        args.extend(["display-message", "-p", CAPTURE_END, ";"]);
    }
    args.pop();
    exec::run("tmux", &args, &[], SLOW)
        .and_then(|out| split_captures(&out, panes.len()))
        .map(|screens| screens.into_iter().map(Some).collect())
        .unwrap_or_else(|| panes.par_iter().map(|pane| capture(pane)).collect())
}

fn split_captures(out: &str, expected: usize) -> Option<Vec<String>> {
    let mut parts: Vec<&str> = out.split(&format!("{CAPTURE_END}\n")).collect();
    let trailing = parts.pop();
    (trailing == Some("") && parts.len() == expected)
        .then(|| parts.into_iter().map(clean_screen).collect())
}

pub fn clean_screen(raw: &str) -> String {
    let text = ANSI.replace_all(raw, "");
    last_nonblank(&text, SCREEN_LINES)
}

/// Coloured capture with scrollback, for the picker preview.
pub fn preview(pane: &str) -> String {
    let raw = tmux(&["capture-pane", "-ep", "-S", "-200", "-t", pane]);
    last_nonblank(&raw, PREVIEW_LINES)
}

/// Last `n` lines that have non-whitespace content (escapes count as content
/// only when they wrap text, so a bare reset sequence line is dropped).
fn last_nonblank(text: &str, n: usize) -> String {
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !ANSI.replace_all(line, "").trim().is_empty())
        .collect();
    kept[kept.len().saturating_sub(n)..].join("\n")
}

/// Focus `pane`: select its window and pane, then switch or attach the client.
///
/// Outside tmux this execs `tmux attach` and only returns on failure.
pub fn switch_to(pane: &str) -> io::Result<()> {
    let window = pane.rsplit_once('.').map_or(pane, |(w, _)| w);
    let session = pane.split_once(':').map_or(pane, |(s, _)| s);
    tmux(&["select-window", "-t", window]);
    tmux(&["select-pane", "-t", pane]);
    if std::env::var_os("TMUX").is_some_and(|v| !v.is_empty()) {
        tmux(&["switch-client", "-t", session]);
        return Ok(());
    }
    Err(Command::new("tmux").args(["attach", "-t", session]).exec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records_with_newlines_in_fields() {
        let listing = "a:1.1\x1fa\x1f42\x1f✳ task\x1f/x/y\x1fh.lan\x1fh\x1e\nb:2.3\x1fb\x1f43\x1fmulti\nline\x1f/p\n\x1fh\x1fh\x1e\n";
        let recs = parse_listing(listing);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].pane, "a:1.1");
        assert_eq!(recs[0].pid, 42);
        assert_eq!(recs[0].hosts, ["h.lan".to_owned(), "h".to_owned()]);
        assert_eq!(recs[1].title, "multi\nline");
        assert_eq!(recs[1].cwd, "/p\n");
    }

    #[test]
    fn decodes_tmux_35_escaped_listing() {
        // Debian tmux 3.5a: separators come back octal-escaped.
        let listing = "t:0.0\\037t\\037169\\037a\\tb\\\\036\\nc\\037/tmp/d\\303\\251j\\303\\240\\03778cc\\03778cc\\036\n\
                       t:1.0\\037t\\037171\\03778cc\\037/tmp\\03778cc\\03778cc\\036\n";
        let recs = parse_listing(&decode_listing(listing));
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].pid, 169);
        assert_eq!(recs[0].title, "a\tb\\036\nc");
        assert_eq!(recs[0].cwd, "/tmp/déjà");
        assert_eq!(recs[1].pane, "t:1.0");
        assert_eq!(recs[1].hosts, ["78cc".to_owned(), "78cc".to_owned()]);
    }

    #[test]
    fn raw_listing_is_not_decoded() {
        let listing = "a:1.1\x1fa\x1f42\x1fback\\037slash\x1f/x\x1fh\x1fh\x1e\n";
        let recs = parse_listing(&decode_listing(listing));
        assert_eq!(recs[0].title, "back\\037slash");
    }

    #[test]
    fn skips_malformed_records() {
        let listing = "a\x1fb\x1e\nx:1.1\x1fx\x1fnotpid\x1ft\x1f/\x1fh\x1fh\x1e\n\x1e";
        assert!(parse_listing(listing).is_empty());
        assert!(parse_listing("").is_empty());
    }

    #[test]
    fn clean_screen_strips_escapes_and_blank_lines() {
        let raw = "\x1b[1;31mred\x1b[0m\n\n   \n\x1b]0;title\x07after\n";
        assert_eq!(clean_screen(raw), "red\nafter");
        let many: String = (0..50).map(|i| format!("l{i}\n")).collect();
        let cleaned = clean_screen(&many);
        assert_eq!(cleaned.lines().count(), 40);
        assert!(cleaned.starts_with("l10\n"));
        assert_eq!(clean_screen(""), "");
    }

    #[test]
    fn splits_chained_captures() {
        let out = format!("a\n\x1b[1mb\x1b[0m\n{CAPTURE_END}\n\n{CAPTURE_END}\n");
        let screens = split_captures(&out, 2).unwrap();
        assert_eq!(screens, ["a\nb", ""]);
        assert_eq!(split_captures(&out, 3), None);
        assert_eq!(
            split_captures(&format!("a\n{CAPTURE_END}\npartial"), 1),
            None
        );
    }

    #[test]
    fn preview_filter_keeps_colour() {
        assert_eq!(
            last_nonblank("\x1b[0m\n\x1b[32mok\x1b[0m\n  \n", 60),
            "\x1b[32mok\x1b[0m"
        );
    }
}
