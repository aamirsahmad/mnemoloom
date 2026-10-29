//! System clipboard via the first command-line tool that works.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::process;

const TIMEOUT: Duration = Duration::from_secs(5);
const WL_COPY: &[&str] = &["wl-copy"];
const TOOLS: [&[&str]; 5] = [
    &["pbcopy"],
    &["xclip", "-selection", "clipboard"],
    &["xsel", "--clipboard", "--input"],
    WL_COPY,
    &["clip"],
];

/// Copy `text`, trying each tool in [`tools`] order.
pub fn copy(text: &str) -> bool {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    tools(wayland).into_iter().any(|argv| copy_with(argv, text))
}

/// `wl-copy` first under Wayland, then pbcopy, xclip, xsel, wl-copy, clip.
fn tools(wayland: bool) -> Vec<&'static [&'static str]> {
    let first = wayland.then_some(WL_COPY);
    first
        .into_iter()
        .chain(TOOLS.into_iter().filter(|t| !(wayland && *t == WL_COPY)))
        .collect()
}

/// stdout is discarded, not piped: xclip and wl-copy fork a selection
/// server that would hold a pipe open.
fn copy_with(argv: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let wrote = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
    let exited = process::wait(child, TIMEOUT);
    wrote && exited.is_ok_and(|(status, _)| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_puts_wl_copy_first_once() {
        let names = |w| tools(w).iter().map(|t| t[0]).collect::<Vec<_>>();
        assert_eq!(names(false), ["pbcopy", "xclip", "xsel", "wl-copy", "clip"]);
        assert_eq!(names(true), ["wl-copy", "pbcopy", "xclip", "xsel", "clip"]);
    }
}
