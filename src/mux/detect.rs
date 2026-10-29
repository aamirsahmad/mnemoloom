//! Screen-scraping heuristics: which harness is this, and what is it doing.
//!
//! Everything here is pure (string in, verdict out) so it can be tested
//! against captured fixtures without a live tmux server.

use std::sync::LazyLock;

use mnemoloom::index::tui::text::prefix;
use regex::Regex;

use super::model::{Kind, State};

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex")
}

/// Interpreters that launch an agent via a script path (Codex ships as JS).
const INTERPRETERS: [&str; 6] = ["node", "node.exe", "python", "python3", "bun", "deno"];

static VERSIONED_BINARY: LazyLock<Regex> = LazyLock::new(|| re(r"^\d+\.\d+\.\d+$"));
static CODEX_PATH: LazyLock<Regex> = LazyLock::new(|| re(r"@openai/codex|/codex(\.js|-cli)?\b"));
static CLAUDE_PATH: LazyLock<Regex> =
    LazyLock::new(|| re(r"/claude(\.js|-code)?\b|/\.local/share/claude/"));
/// `pi` is too common a word for a loose match: only its npm package or the
/// `node_modules/.bin/pi` shim (the running TUI retitles itself to `pi`).
static PI_PATH: LazyLock<Regex> =
    LazyLock::new(|| re(r"/pi-coding-agent/|/node_modules/\.bin/pi$"));
static OPENCODE_PATH: LazyLock<Regex> =
    LazyLock::new(|| re(r"/opencode-ai/|/node_modules/\.bin/opencode$"));

/// Identify the harness from process command lines (one per line).
///
/// Only the executable decides -- argv[0], or the script path when argv[0] is
/// an interpreter -- so `rg claude .` is not an agent. First matching line wins.
pub fn classify_argv(argv: &str) -> Option<Kind> {
    argv.lines().find_map(classify_command)
}

fn classify_command(line: &str) -> Option<Kind> {
    let mut tokens = line.split_whitespace();
    let exe = basename(tokens.next()?);
    match exe {
        "codex" | "codex-cli" => return Some(Kind::Codex),
        "claude" | "claude-code" => return Some(Kind::Claude),
        "pi" => return Some(Kind::Pi),
        "opencode" => return Some(Kind::OpenCode),
        _ if VERSIONED_BINARY.is_match(exe) => return Some(Kind::Claude),
        _ => {}
    }
    if !INTERPRETERS.contains(&exe) {
        return None;
    }
    let script = tokens.next()?;
    if CODEX_PATH.is_match(script) {
        Some(Kind::Codex)
    } else if CLAUDE_PATH.is_match(script) {
        Some(Kind::Claude)
    } else if PI_PATH.is_match(script) {
        Some(Kind::Pi)
    } else if OPENCODE_PATH.is_match(script) {
        Some(Kind::OpenCode)
    } else {
        None
    }
}

/// Executable name from an argv[0] token.
pub fn basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// Claude's numbered choices and permission prompts.
static ASKING: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?m)❯\s*1\.|Do you want|Would you like|^\s*1\.\s*Yes|\(y/n\)|Select an option|Choose an option",
    )
});
/// The main agent is mid-turn (a subagent row has the counter without parens).
static BUSY: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)esc to interrupt|\(\d+[ms][^)]*↓[^)]*tokens"));
/// Monitors, background shells, or a running (◯) subagent.
static BG: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?m)\b[1-9]\d*\s+monitors?\b|\b[1-9]\d*\s+shells? still running\b|^\s*⏵⏵.*·\s*[1-9]\d*\s+shells?\b|^\s*◯\s+\S",
    )
});
/// pi's working loader: `Working (escape to interrupt)`, key per keybindings.
static PI_BUSY: LazyLock<Regex> = LazyLock::new(|| re(r"\(\S+ to interrupt\)"));
/// opencode's prompt footer while a turn runs: `esc interrupt`, then
/// `esc again to interrupt` after one press.
static OPENCODE_BUSY: LazyLock<Regex> = LazyLock::new(|| re(r"\besc (?:again to )?interrupt\b"));
/// opencode's tool permission dialog.
static OPENCODE_ASKING: LazyLock<Regex> = LazyLock::new(|| re(r"Permission required|Allow once"));
static IDLE_SUBAGENT: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*◯.*\bidle\s*$"));
/// Footer chrome rendered below a completed exchange, anchored at line start.
/// The `gpt-` prefix is case-insensitive: current Codex prints `GPT-6 Astra`.
static FOOTER: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*(⏵⏵|ctx [█░]|new task\?|✻|※|(?i:gpt)-[\w.\-]+\s|›)"));
/// Codex chrome to ignore when looking for its last content line.
static CODEX_CHROME: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*[›❯]|·|(?i:gpt)-[\d.]|^\s*─|esc to|tab to|^\s*$|^\s*[▌│]"));

/// A pending prompt sits at the bottom of the screen.
const PROMPT_TAIL_LINES: usize = 12;

/// Classify what the agent is doing from its visible screen.
pub fn detect_state(screen: &str, kind: Kind) -> State {
    let lines: Vec<&str> = screen.lines().collect();
    let tail_lines = &lines[lines.len().saturating_sub(PROMPT_TAIL_LINES)..];
    let tail = tail_lines.join("\n");
    if (ASKING.is_match(&tail) && !footer_below_prompt(tail_lines))
        || (kind == Kind::OpenCode && OPENCODE_ASKING.is_match(&tail))
    {
        return State::Input;
    }
    // A visible spinner beats the Codex question heuristic: the question may
    // already be answered and the agent working on it.
    let kind_busy = match kind {
        Kind::Pi => PI_BUSY.is_match(&tail),
        Kind::OpenCode => OPENCODE_BUSY.is_match(&tail),
        Kind::Claude | Kind::Codex => false,
    };
    if kind_busy || BUSY.is_match(screen) {
        return State::Busy;
    }
    if kind == Kind::Codex && codex_awaiting_answer(tail_lines) {
        return State::Input;
    }
    let background: Vec<&str> = lines
        .iter()
        .filter(|line| !IDLE_SUBAGENT.is_match(line))
        .map(|line| line.trim_end())
        .collect();
    if BG.is_match(&background.join("\n")) {
        return State::Bg;
    }
    State::Idle
}

/// Whether footer chrome appears after the last prompt line (prompt answered).
fn footer_below_prompt(lines: &[&str]) -> bool {
    let Some(last) = lines.iter().rposition(|line| ASKING.is_match(line)) else {
        return false;
    };
    lines[last + 1..].iter().any(|line| FOOTER.is_match(line))
}

/// Codex has no prompt UI: the last real content line ending in `?` is the signal.
fn codex_awaiting_answer(tail: &[&str]) -> bool {
    tail.iter()
        .rev()
        .find(|line| !line.trim().is_empty() && !CODEX_CHROME.is_match(line))
        .is_some_and(|line| line.trim_end().ends_with('?'))
}

/// Codex status line. Matches the old `gpt-5.6-sol high · …` and the current
/// `GPT-6.1 Sol high · …` (capitalised words after the version belong to the model).
static CODEX_STATUS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"((?i:gpt)-[\w.\-]+(?:[ \t]+[A-Z][\w.\-]*)*)\s+(\w+)?\s*·\s*([^·]+)·\s*([^·\n]+)")
});
static CLAUDE_MODEL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(opus[\w.\-\[\]]*|sonnet[\w.\-\[\]]*|haiku[\w.\-\[\]]*|fable[\w.\-\[\]]*)")
});
static SEPARATOR_NAME: LazyLock<Regex> =
    LazyLock::new(|| re(r"─\s([A-Za-z0-9][A-Za-z0-9._-]{3,})\s─"));
static ARGV_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"--(?:resume|name)[= ](\S+)"));
static SPINNER: LazyLock<Regex> = LazyLock::new(|| re(r"^[✳*⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏\s]+"));
static SHELL_TITLE: LazyLock<Regex> = LazyLock::new(|| re(r"[/\[\]]|\.lan$|\.local$"));
static MULTI_SPACE: LazyLock<Regex> = LazyLock::new(|| re(r"\s{2,}"));

/// pi's footer stats line: token counters, then `ctx%/window (auto)`, then
/// right-aligned `(provider) model • thinking` (provider only when several
/// are configured, thinking only for reasoning models).
static PI_STATS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?:[\d.]+%|\?)/[\d.]+[kKmM]?(?: \(auto\))?\s{2,}(?:\([^)\s]+\) )?(\S+)(?: • ([^→]+?))?(?: → .*)?\s*$",
    )
});

/// pi's stats line and the `pwd (branch)` line above it, from the bottom.
fn pi_footer(screen: &str) -> Option<(regex::Captures<'_>, &str)> {
    let lines: Vec<&str> = screen.lines().collect();
    lines.iter().enumerate().rev().find_map(|(i, line)| {
        let c = PI_STATS.captures(line)?;
        let pwd = i.checked_sub(1).map_or("", |j| lines[j].trim());
        Some((c, pwd))
    })
}

/// The Codex status line, searched from the bottom so that prose above the
/// footer (say, a table row naming a GPT model) cannot win.
fn codex_status(screen: &str) -> Option<regex::Captures<'_>> {
    screen
        .lines()
        .rev()
        .find_map(|line| CODEX_STATUS.captures(line))
}

/// Model identifier from the harness footer, if present.
pub fn extract_model(screen: &str, kind: Kind) -> String {
    let found = match kind {
        Kind::Codex => codex_status(screen).and_then(|c| c.get(1)),
        Kind::Claude => CLAUDE_MODEL.captures(screen).and_then(|c| c.get(1)),
        Kind::Pi => pi_footer(screen).and_then(|(c, _)| c.get(1)),
        // No recognisable model footer.
        Kind::OpenCode => None,
    };
    found.map(|m| m.as_str().to_owned()).unwrap_or_default()
}

/// Session name: the agent's own argv `--resume/--name`, then the pane title
/// (unless it is a shell path or tmux's default, the hostname), then an
/// on-screen separator line.
pub fn extract_name(agent_argv: &str, pane_title: &str, screen: &str, hosts: &[&str]) -> String {
    if let Some(c) = ARGV_NAME.captures(agent_argv) {
        return c[1].to_owned();
    }
    let title = SPINNER.replace(pane_title, "");
    let title = title.trim();
    let is_host = hosts.iter().any(|h| !h.is_empty() && *h == title);
    if !title.is_empty() && !is_host && !SHELL_TITLE.is_match(title) {
        return title.to_owned();
    }
    SEPARATOR_NAME
        .captures_iter(screen)
        .last()
        .map(|c| c[1].to_owned())
        .unwrap_or_default()
}

/// One line of context for the list view (model, repo, branch, …).
pub fn extract_info(screen: &str, kind: Kind) -> String {
    if kind == Kind::Codex
        && let Some(c) = codex_status(screen)
    {
        let parts: Vec<&str> = c
            .iter()
            .skip(1)
            .flatten()
            .map(|m| m.as_str().trim())
            .collect();
        return parts.join(" · ");
    }
    match kind {
        Kind::Pi => {
            return pi_footer(screen).map_or_else(String::new, |(c, pwd)| {
                let parts: Vec<&str> = c
                    .iter()
                    .skip(1)
                    .flatten()
                    .map(|m| m.as_str().trim())
                    .chain(Some(pwd).filter(|p| !p.is_empty()))
                    .collect();
                prefix(&parts.join(" · "), 80).to_owned()
            });
        }
        // No recognisable footer: the last screen line would be prose.
        Kind::OpenCode => return String::new(),
        Kind::Claude | Kind::Codex => {}
    }
    let lines: Vec<&str> = screen
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if let Some(line) = lines.iter().rev().find(|l| CLAUDE_MODEL.is_match(l)) {
        return prefix(&MULTI_SPACE.replace_all(line, " "), 80).to_owned();
    }
    lines
        .last()
        .map(|l| prefix(l, 80).to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_IDLE: &str = "  I've committed the change and pushed the branch.
──────────────────────────────────────────────── release-notes ──
❯
──────────────────────────────────────────────────────────────
   fable   webapp.feat-login  feat/login
  ctx ████████░░ 86%   5h ░░░░░░░░░░ 3% ↻4h29m
  ⏵⏵ bypass permissions on · ← 1 agent";

    const CODEX_IDLE: &str = "• Explored…
─ Worked for 2m 04s ──
› Write tests for @filename
  gpt-5.6-sol high · docs · main · Context 55% used";

    const CODEX_BUSY: &str = "• Waiting for background terminal (1m 02s • esc to interrupt)
  gpt-5.6-sol xhigh · infra · main";

    const CODEX_ASKING: &str = "• I found two candidate configs.
  Should I delete the stale one before continuing?
›
  gpt-5.6-sol high · api · main";

    const CODEX_NEW_FOOTER: &str = "• Done.
› Ask Codex to do anything
  GPT-6 Astra high · ~/src/api · Plan event ingestion service · Main [default]
  enter/esc latest · ? shortcuts";

    const PI_IDLE: &str = " Set the default model in the config file.

────────────────────────────────────────────────────────────────

────────────────────────────────────────────────────────────────
~/src/webapp (main)
↑72 ↓3.6k R239k W30k CH99.2% 1.9%/1.0M (auto)              (acme) gpt-6.1-sol • high";

    const PI_BUSY: &str = " Reading files.

⠋ Working (escape to interrupt)
────────────────────────────────────────────────────────────────
~/src/webapp
↑1.2k ↓300 4.0%/200k              sonnet-5";

    const OPENCODE_BUSY: &str = "  Thinking about the migration…
┃
┃  Build  Example Model 4
╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
   ⬝⬝⬝■■  esc interrupt                    ctrl+p commands";

    const OPENCODE_ASKING: &str = "  △ Permission required
    $ rm -rf build/
   Allow once   Allow always   Reject";

    #[test]
    fn classify_pi_and_opencode() {
        let cases = [
            ("pi", Kind::Pi),
            ("pi ", Kind::Pi),
            ("/home/alice/.pi/agent/bin/pi --session x", Kind::Pi),
            (
                "node /home/alice/.pi/agent/install/releases/1.4.0/node_modules/.bin/pi",
                Kind::Pi,
            ),
            (
                "node /opt/lib/node_modules/@example/pi-coding-agent/dist/bundle/cli.js",
                Kind::Pi,
            ),
            ("-zsh\npi", Kind::Pi),
            ("opencode", Kind::OpenCode),
            ("/opt/homebrew/bin/opencode -s ses_abc", Kind::OpenCode),
            (
                "node /usr/lib/node_modules/opencode-ai/bin/opencode",
                Kind::OpenCode,
            ),
            ("bun /x/node_modules/.bin/opencode", Kind::OpenCode),
        ];
        for (argv, kind) in cases {
            assert_eq!(classify_argv(argv), Some(kind), "{argv}");
        }
    }

    #[test]
    fn short_pi_name_has_no_false_positives() {
        for argv in [
            "pip install requests",
            "/usr/bin/pip3 list",
            "pi-rpc",
            "pinentry-mac",
            "vim pi.md",
            "rg pi .",
            "cat /home/alice/pi/notes",
            "python3 /home/alice/pi/run.py",
            "python3 pi",
            "node /srv/pi/server.js",
            "node /srv/api/pi.js",
            "less opencode.log",
            "node /srv/opencode-notes/index.js",
            "opencode-server-proxy",
        ] {
            assert_eq!(classify_argv(argv), None, "{argv}");
        }
    }

    #[test]
    fn pi_and_opencode_states() {
        assert_eq!(detect_state(PI_IDLE, Kind::Pi), State::Idle);
        assert_eq!(detect_state(PI_BUSY, Kind::Pi), State::Busy);
        assert_eq!(detect_state(OPENCODE_BUSY, Kind::OpenCode), State::Busy);
        let again = OPENCODE_BUSY.replace("esc interrupt", "esc again to interrupt");
        assert_eq!(detect_state(&again, Kind::OpenCode), State::Busy);
        let idle = OPENCODE_BUSY.replace("esc interrupt", "tab agents");
        assert_eq!(detect_state(&idle, Kind::OpenCode), State::Idle);
        assert_eq!(detect_state(OPENCODE_ASKING, Kind::OpenCode), State::Input);
        // Kind-specific markers do not leak across harnesses.
        assert_eq!(detect_state(OPENCODE_ASKING, Kind::Pi), State::Idle);
        assert_eq!(detect_state(PI_BUSY, Kind::OpenCode), State::Idle);
    }

    #[test]
    fn pi_model_and_info() {
        assert_eq!(extract_model(PI_IDLE, Kind::Pi), "gpt-6.1-sol");
        assert_eq!(
            extract_info(PI_IDLE, Kind::Pi),
            "gpt-6.1-sol · high · ~/src/webapp (main)"
        );
        assert_eq!(extract_model(PI_BUSY, Kind::Pi), "sonnet-5");
        assert_eq!(extract_info(PI_BUSY, Kind::Pi), "sonnet-5 · ~/src/webapp");
        let routed = "~/w\n↑1 ?/1.0M (auto)  auto • high → gpt-6.1-sol • low";
        assert_eq!(extract_info(routed, Kind::Pi), "auto · high · ~/w");
        assert_eq!(extract_info("plain prose 5%/1.0M", Kind::Pi), "");
        assert_eq!(extract_model(OPENCODE_BUSY, Kind::OpenCode), "");
        assert_eq!(extract_info(OPENCODE_BUSY, Kind::OpenCode), "");
    }

    #[test]
    fn classify_argv_positive() {
        let cases = [
            ("codex --yolo", Kind::Codex),
            ("/opt/bin/codex-cli", Kind::Codex),
            ("claude --resume x", Kind::Claude),
            (
                "/home/alice/.local/share/claude/versions/2.1.220 --x",
                Kind::Claude,
            ),
            (
                "node /usr/lib/node_modules/@openai/codex/bin/codex.js",
                Kind::Codex,
            ),
            ("node /usr/local/bin/claude", Kind::Claude),
            ("bun /x/.local/share/claude/cli.js", Kind::Claude),
            ("-zsh\ndirenv exec . zsh\nclaude", Kind::Claude),
            ("zsh\ncodex\nclaude", Kind::Codex),
        ];
        for (argv, kind) in cases {
            assert_eq!(classify_argv(argv), Some(kind), "{argv}");
        }
    }

    #[test]
    fn classify_argv_ignores_arguments() {
        for argv in [
            "rg claude .",
            "vim claude_notes.md",
            "git commit -m 'fix codex'",
            "node server.js codex",
            "",
            "   ",
            "python3",
        ] {
            assert_eq!(classify_argv(argv), None, "{argv}");
        }
    }

    #[test]
    fn claude_states() {
        assert_eq!(detect_state(CLAUDE_IDLE, Kind::Claude), State::Idle);
        let bg = CLAUDE_IDLE.replace("· ← 1 agent", "· 1 monitor · ← 1 agent");
        assert_eq!(detect_state(&bg, Kind::Claude), State::Bg);
        let cases = [
            ("✻ Cooked for 14m 17s · esc to interrupt", State::Busy),
            (
                "· Razzmatazzing… (1m 42s · ↓ 4.8k tokens · thought for 14s)",
                State::Busy,
            ),
            (
                "  ◯ cache-incremental-evidence  Grepping …   1m 16s · ↓ 873.5k tokens",
                State::Bg,
            ),
            ("◯ chk4 Checking … idle  ", State::Idle),
            ("✻ Baked for 16m 24s · 1 monitor still running", State::Bg),
            ("✻ Baked for 42m · 1 shell still running", State::Bg),
            ("  ⏵⏵ bypass permissions on · 2 shells", State::Bg),
            ("Ran 1 shell command", State::Idle),
            (
                "⏺ The keepalive is built and done.\n· ← 1 agent",
                State::Idle,
            ),
            (
                "❯ 1. Rebase onto main\n  2. Merge main in\n  3. Skip for now",
                State::Input,
            ),
        ];
        for (screen, state) in cases {
            assert_eq!(detect_state(screen, Kind::Claude), state, "{screen}");
        }
    }

    #[test]
    fn answered_prompt_with_spinner_below_is_busy() {
        let screen = "Do you want to proceed?\n❯ 1. Yes\n  2. No\n✻ Working… (12s · ↓ 1.2k tokens)";
        assert_eq!(detect_state(screen, Kind::Claude), State::Busy);
    }

    #[test]
    fn pending_prompt_beats_spinner_above() {
        let screen = "✻ Cooked for 1m · esc to interrupt\nDo you want to proceed?\n❯ 1. Yes\n  2. Explain the ⏵⏵ bypass";
        assert_eq!(detect_state(screen, Kind::Claude), State::Input);
    }

    #[test]
    fn prompt_above_tail_is_ignored() {
        let mut screen = String::from("Would you like me to continue?\n");
        for i in 0..12 {
            screen.push_str(&format!("statement {i}\n"));
        }
        assert_eq!(detect_state(&screen, Kind::Claude), State::Idle);
        assert_eq!(detect_state(&screen, Kind::Codex), State::Idle);
    }

    #[test]
    fn codex_states() {
        assert_eq!(detect_state(CODEX_IDLE, Kind::Codex), State::Idle);
        assert_eq!(detect_state(CODEX_BUSY, Kind::Codex), State::Busy);
        assert_eq!(detect_state(CODEX_ASKING, Kind::Codex), State::Input);
        assert_eq!(detect_state(CODEX_NEW_FOOTER, Kind::Codex), State::Idle);
        let busy_after_question = format!("{CODEX_ASKING}\n• Running (3s • esc to interrupt)");
        assert_eq!(detect_state(&busy_after_question, Kind::Codex), State::Busy);
    }

    #[test]
    fn codex_model_and_info() {
        assert_eq!(extract_model(CODEX_IDLE, Kind::Codex), "gpt-5.6-sol");
        assert_eq!(
            extract_info(CODEX_IDLE, Kind::Codex),
            "gpt-5.6-sol · high · docs · main"
        );
        assert_eq!(extract_model(CODEX_NEW_FOOTER, Kind::Codex), "GPT-6 Astra");
        assert_eq!(
            extract_info(CODEX_NEW_FOOTER, Kind::Codex),
            "GPT-6 Astra · high · ~/src/api · Plan event ingestion service"
        );
        let sol = "  GPT-6.1 Sol high · ~/src/infra · Check access\n  ? for shortcuts";
        assert_eq!(extract_model(sol, Kind::Codex), "GPT-6.1 Sol");
    }

    #[test]
    fn claude_model_and_info() {
        assert_eq!(extract_model(CLAUDE_IDLE, Kind::Claude), "fable");
        assert_eq!(
            extract_info(CLAUDE_IDLE, Kind::Claude),
            "fable webapp.feat-login feat/login"
        );
        assert_eq!(extract_info("", Kind::Claude), "");
        let long = "x".repeat(100);
        assert_eq!(extract_info(&long, Kind::Claude).chars().count(), 80);
        assert_eq!(
            extract_info(&"日".repeat(100), Kind::Claude),
            "日".repeat(40)
        );
        assert_eq!(
            extract_info(&"😀".repeat(100), Kind::Claude),
            "😀".repeat(40)
        );
    }

    #[test]
    fn hostname_title_is_not_a_name() {
        let hosts = ["alices-laptop.corp.example", "alices-laptop"];
        assert_eq!(
            extract_name("claude", "alices-laptop", CLAUDE_IDLE, &hosts),
            "release-notes"
        );
        assert_eq!(
            extract_name("claude", "✳ Real task", "", &hosts),
            "Real task"
        );
        assert_eq!(extract_name("claude", "box", "", &["", "box"]), "");
    }

    #[test]
    fn codex_footer_beats_prose_above_it() {
        let screen = format!("| GPT-5 Pro · fast · cheap · old |\n{CODEX_NEW_FOOTER}");
        assert_eq!(extract_model(&screen, Kind::Codex), "GPT-6 Astra");
        assert!(extract_info(&screen, Kind::Codex).starts_with("GPT-6 Astra · high"));
    }

    #[test]
    fn name_precedence() {
        let extract_name = |argv, title, screen| extract_name(argv, title, screen, &[]);
        assert_eq!(
            extract_name("claude --resume abc", "✳ title", CLAUDE_IDLE),
            "abc"
        );
        assert_eq!(extract_name("claude --name=foo", "", ""), "foo");
        assert_eq!(extract_name("claude", "✳ My task", CLAUDE_IDLE), "My task");
        assert_eq!(
            extract_name("claude", "~/Git/foo", CLAUDE_IDLE),
            "release-notes"
        );
        assert_eq!(extract_name("claude", "host.lan", ""), "");
        assert_eq!(extract_name("claude", "⠋ ", "─ ab ─"), "");
    }
}
