//! Resume primitives: `claude -r <id>` / `codex resume <id>` /
//! `pi --session <file>` / `opencode -s <id>` in the session's cwd, by
//! `execvp` (replaces this process) or as a child.

use std::ffi::{OsStr, OsString};
use std::io::{self, BufRead, IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus};

use anyhow::{Context, bail};

use super::{Agent, Homes};

/// Which Claude resume flag to use (both are used in different places).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaudeFlag {
    /// `claude -r <id>` (resume / menu).
    #[default]
    Short,
    /// `claude --resume <id>` (move, trim, rollover).
    Long,
}

/// The agent executable: `MNEMOLOOM_{CLAUDE,CODEX,PI,OPENCODE}_BIN`, else
/// `claude` / `codex` / `pi` / `opencode`.
pub fn agent_bin(agent: Agent) -> OsString {
    let var = match agent {
        Agent::Claude => "MNEMOLOOM_CLAUDE_BIN",
        Agent::Codex => "MNEMOLOOM_CODEX_BIN",
        Agent::Pi => "MNEMOLOOM_PI_BIN",
        Agent::OpenCode => "MNEMOLOOM_OPENCODE_BIN",
    };
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| agent.as_str().into())
}

/// The resume argv: `[<claude>, "-r"|"--resume", id]` / `[<codex>, "resume", id]`
/// / `[<pi>, "--session", file]` / `[<opencode>, "-s", id]`. `session_id` is
/// the agent's resume target ([`super::resume_target`]: the file for pi).
pub fn resume_argv(agent: Agent, session_id: &str, flag: ClaudeFlag) -> Vec<OsString> {
    resume_argv_with(agent_bin(agent), agent, session_id, flag)
}

/// [`resume_argv`] with an explicit executable.
fn resume_argv_with(
    bin: OsString,
    agent: Agent,
    session_id: &str,
    flag: ClaudeFlag,
) -> Vec<OsString> {
    let sub = match (agent, flag) {
        (Agent::Claude, ClaudeFlag::Short) => "-r",
        (Agent::Claude, ClaudeFlag::Long) => "--resume",
        (Agent::Codex, _) => "resume",
        (Agent::Pi, _) => "--session",
        (Agent::OpenCode, _) => "-s",
    };
    vec![bin, sub.into(), session_id.into()]
}

/// The home variable the agent must see: `CLAUDE_CONFIG_DIR` / `CODEX_HOME`
/// / `PI_CODING_AGENT_DIR` when that agent's home was overridden (flag or
/// env), so it looks in the same home the session was resolved from. An
/// overridden opencode home `<dir>/opencode` exports `XDG_DATA_HOME=<dir>`
/// (opencode has no data-dir variable of its own).
fn resume_env(agent: Agent, homes: &Homes) -> Option<(&'static str, &Path)> {
    match agent {
        Agent::Claude if homes.claude_overridden => Some(("CLAUDE_CONFIG_DIR", &homes.claude)),
        Agent::Codex if homes.codex_overridden => Some(("CODEX_HOME", &homes.codex)),
        Agent::Pi if homes.pi_overridden => Some(("PI_CODING_AGENT_DIR", &homes.pi)),
        Agent::OpenCode
            if homes.opencode_overridden
                && homes.opencode.file_name().is_some_and(|n| n == "opencode") =>
        {
            homes.opencode.parent().map(|p| ("XDG_DATA_HOME", p))
        }
        _ => None,
    }
}

/// POSIX shell quoting (Python `shlex.quote`).
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@%+=:,./-_".contains(&b))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

/// The shell form of [`resume_command`]: `cd <cwd> && [VAR=<home>] claude
/// --resume <id>` (each word quoted).
pub fn resume_hint(
    agent: Agent,
    session_id: &str,
    cwd: Option<&str>,
    flag: ClaudeFlag,
    homes: &Homes,
) -> String {
    command_hint(agent, &resume_argv(agent, session_id, flag), cwd, homes)
}

/// The shell form of running `argv` (program first) as `agent`: `cd <cwd>
/// && [VAR=<home>] <argv>` (each word quoted).
pub fn command_hint<S: AsRef<OsStr>>(
    agent: Agent,
    argv: &[S],
    cwd: Option<&str>,
    homes: &Homes,
) -> String {
    let quote = |s: &OsStr| shell_quote(&s.to_string_lossy());
    let mut words: Vec<String> = resume_env(agent, homes)
        .map(|(k, v)| format!("{k}={}", quote(v.as_os_str())))
        .into_iter()
        .collect();
    words.extend(argv.iter().map(|a| quote(a.as_ref())));
    let cmd = words.join(" ");
    match cwd.filter(|c| !c.is_empty()) {
        Some(c) => format!("cd {} && {cmd}", shell_quote(c)),
        None => cmd,
    }
}

/// The interactive-launch guard: without a terminal, print `hint` (the
/// command to run instead) and return false; true = go ahead and launch.
pub fn may_launch(agent: Agent, interactive: bool, hint: impl FnOnce() -> String) -> bool {
    if !interactive {
        println!(
            "\nNot launching {} without a terminal. To resume, run:\n  {}",
            agent.label(),
            hint()
        );
    }
    interactive
}

/// Read one line from stdin after printing `prompt` to stdout; `None` on EOF.
pub fn prompt_line(prompt: &str) -> io::Result<Option<String>> {
    prompt_from(&mut io::stdin().lock(), prompt)
}

/// [`prompt_line`] reading from `input`.
pub fn prompt_from(input: &mut impl BufRead, prompt: &str) -> io::Result<Option<String>> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

/// How to launch.
#[derive(Debug, Clone, Copy, Default)]
pub struct LaunchOptions {
    pub claude_flag: ClaudeFlag,
    /// Ask before changing to the session's cwd when it differs from the
    /// process cwd. When false, change silently.
    pub confirm_chdir: bool,
}

/// The resume [`Command`] after the cwd handling, or `None` when nothing
/// should be launched: without an interactive terminal (prints the resume
/// hint instead) or when the chdir prompt hits EOF.
fn prepare(
    agent: Agent,
    session_id: &str,
    cwd: Option<&str>,
    homes: &Homes,
    opts: LaunchOptions,
    input: &mut impl BufRead,
    interactive: bool,
) -> anyhow::Result<Option<Command>> {
    if !may_launch(agent, interactive, || {
        resume_hint(agent, session_id, cwd, opts.claude_flag, homes)
    }) {
        return Ok(None);
    }
    let current = std::env::current_dir().ok();
    if let Some(dir) = cwd.filter(|c| !c.is_empty())
        && current.as_deref() != Some(Path::new(dir))
    {
        let change = if !opts.confirm_chdir {
            true
        } else if agent == Agent::Claude {
            println!("\nThis session is from a different project:");
            println!(
                "  Current directory: {}",
                current.as_deref().unwrap_or(Path::new("?")).display()
            );
            println!("  Session directory: {dir}");
            let Some(ans) = prompt_from(input, "\nChange to the session's directory? [Y/n]: ")?
            else {
                println!("\nCancelled.");
                return Ok(None);
            };
            if ans.eq_ignore_ascii_case("n") || ans.eq_ignore_ascii_case("no") {
                println!("Staying in current directory. Session resume may fail.");
                false
            } else {
                true
            }
        } else {
            let Some(ans) = prompt_from(
                input,
                &format!(
                    "\nSession is in different directory: {dir}\nChange directory and resume? [Y/n]: "
                ),
            )?
            else {
                println!("\nCancelled.");
                return Ok(None);
            };
            matches!(ans.to_lowercase().as_str(), "" | "y" | "yes")
        };
        if change {
            std::env::set_current_dir(dir)
                .with_context(|| format!("changing directory to {dir}"))?;
            if agent == Agent::Codex && opts.confirm_chdir {
                println!("Changed to: {dir}");
            }
        }
    }
    if agent == Agent::Claude && opts.confirm_chdir {
        println!("\nResuming session: {session_id}");
    }
    Ok(Some(resume_command(
        agent,
        session_id,
        homes,
        opts.claude_flag,
    )))
}

/// [`prepare`] on the real stdin.
fn prepare_stdin(
    agent: Agent,
    session_id: &str,
    cwd: Option<&str>,
    homes: &Homes,
    opts: LaunchOptions,
) -> anyhow::Result<Option<Command>> {
    let stdin = io::stdin();
    let interactive = stdin.is_terminal();
    prepare(
        agent,
        session_id,
        cwd,
        homes,
        opts,
        &mut stdin.lock(),
        interactive,
    )
}

/// The resume [`Command`] (no cwd handling), with the home variable from
/// [`resume_env`].
pub fn resume_command(agent: Agent, session_id: &str, homes: &Homes, flag: ClaudeFlag) -> Command {
    let argv = resume_argv(agent, session_id, flag);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Some((k, v)) = resume_env(agent, homes) {
        cmd.env(k, v);
    }
    cmd
}

/// Replace this process with the agent's resume command (execvp). Returns
/// `Ok` without launching when stdin is not a terminal (the resume hint is
/// printed instead) or the chdir prompt is cancelled; otherwise only
/// returns on failure.
pub fn exec_resume(
    agent: Agent,
    session_id: &str,
    cwd: Option<&str>,
    homes: &Homes,
    opts: LaunchOptions,
) -> anyhow::Result<()> {
    let Some(mut cmd) = prepare_stdin(agent, session_id, cwd, homes, opts)? else {
        return Ok(());
    };
    let err = cmd.exec();
    if err.kind() == io::ErrorKind::NotFound {
        bail!(
            "'{}' command not found. Make sure the {} CLI is installed.",
            agent_bin(agent).to_string_lossy(),
            agent.label()
        );
    }
    Err(err).context("launching agent")
}

/// Run the resume command as a child and wait.
/// `None` when nothing was launched (see [`exec_resume`]).
pub fn spawn_resume(
    agent: Agent,
    session_id: &str,
    cwd: Option<&str>,
    homes: &Homes,
    opts: LaunchOptions,
) -> anyhow::Result<Option<ExitStatus>> {
    prepare_stdin(agent, session_id, cwd, homes, opts)?
        .map(|mut cmd| cmd.status().context("launching agent"))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_overridden_homes() {
        let env_of = |c: &Command, k: &str| {
            c.get_envs()
                .find(|(n, _)| *n == k)
                .and_then(|(_, v)| v.map(|v| v.to_os_string()))
        };
        let homes = Homes::new("/h/claude", "/h/codex");
        let c = resume_command(Agent::Codex, "x", &homes, ClaudeFlag::Short);
        assert_eq!(env_of(&c, "CODEX_HOME"), Some("/h/codex".into()));
        assert_eq!(env_of(&c, "CLAUDE_CONFIG_DIR"), None);
        let c = resume_command(Agent::Claude, "x", &homes, ClaudeFlag::Short);
        assert_eq!(env_of(&c, "CLAUDE_CONFIG_DIR"), Some("/h/claude".into()));
        let plain = Homes {
            claude_overridden: false,
            codex_overridden: false,
            ..homes
        };
        let c = resume_command(Agent::Codex, "x", &plain, ClaudeFlag::Short);
        assert_eq!(env_of(&c, "CODEX_HOME"), None);
        let more = plain.with_pi("/h/pi").with_opencode("/h/data/opencode");
        let c = resume_command(Agent::Pi, "/s.jsonl", &more, ClaudeFlag::Short);
        assert_eq!(env_of(&c, "PI_CODING_AGENT_DIR"), Some("/h/pi".into()));
        let c = resume_command(Agent::OpenCode, "ses_1", &more, ClaudeFlag::Short);
        assert_eq!(env_of(&c, "XDG_DATA_HOME"), Some("/h/data".into()));
        let odd = Homes::new("/c", "/x").with_opencode("/h/oc-data");
        let c = resume_command(Agent::OpenCode, "ses_1", &odd, ClaudeFlag::Short);
        assert_eq!(env_of(&c, "XDG_DATA_HOME"), None);
    }

    /// Hint for an explicit `claude`/`codex` (independent of `MNEMOLOOM_*_BIN`).
    fn hint(agent: Agent, cwd: Option<&str>, flag: ClaudeFlag, homes: &Homes) -> String {
        let argv = resume_argv_with(agent.as_str().into(), agent, "x", flag);
        command_hint(agent, &argv, cwd, homes)
    }

    #[test]
    fn argv_and_hints() {
        let argv = |agent: Agent, flag| resume_argv_with(agent.as_str().into(), agent, "x", flag);
        assert_eq!(
            argv(Agent::Claude, ClaudeFlag::Short),
            ["claude", "-r", "x"]
        );
        assert_eq!(
            argv(Agent::Claude, ClaudeFlag::Long),
            ["claude", "--resume", "x"]
        );
        assert_eq!(
            argv(Agent::Codex, ClaudeFlag::Long),
            ["codex", "resume", "x"]
        );
        assert_eq!(
            resume_argv(Agent::Codex, "x", ClaudeFlag::Long)[0],
            agent_bin(Agent::Codex)
        );
        let plain = Homes {
            claude_overridden: false,
            codex_overridden: false,
            ..Homes::new("/h/claude", "/h/codex")
        };
        assert_eq!(
            hint(Agent::Claude, Some("/a b"), ClaudeFlag::Long, &plain),
            "cd '/a b' && claude --resume x"
        );
        assert_eq!(
            hint(Agent::Codex, None, ClaudeFlag::Long, &plain),
            "codex resume x"
        );
        assert_eq!(argv(Agent::Pi, ClaudeFlag::Short), ["pi", "--session", "x"]);
        assert_eq!(
            argv(Agent::OpenCode, ClaudeFlag::Short),
            ["opencode", "-s", "x"]
        );
        assert_eq!(
            hint(Agent::OpenCode, Some("/p"), ClaudeFlag::Short, &plain),
            "cd /p && opencode -s x"
        );
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("/a/b-c.d"), "/a/b-c.d");
    }

    #[test]
    fn hints_export_overridden_homes() {
        let homes = Homes::new("/h/my claude", "/h/codex");
        assert_eq!(
            hint(Agent::Claude, Some("/p"), ClaudeFlag::Long, &homes),
            "cd /p && CLAUDE_CONFIG_DIR='/h/my claude' claude --resume x"
        );
        assert_eq!(
            hint(Agent::Codex, None, ClaudeFlag::Short, &homes),
            "CODEX_HOME=/h/codex codex resume x"
        );
        let quoted = command_hint(
            Agent::Codex,
            &["/b in/codex", "-c", "model=m"],
            None,
            &homes,
        );
        assert_eq!(quoted, "CODEX_HOME=/h/codex '/b in/codex' -c model=m");
    }

    fn prepared(input: &str, interactive: bool, opts: LaunchOptions) -> Option<Command> {
        let homes = Homes::new("/h/claude", "/h/codex");
        let elsewhere = "/definitely/not/the/test/cwd";
        prepare(
            Agent::Claude,
            "x",
            Some(elsewhere),
            &homes,
            opts,
            &mut input.as_bytes(),
            interactive,
        )
        .unwrap()
    }

    #[test]
    fn never_launches_without_a_terminal() {
        let confirm = LaunchOptions {
            confirm_chdir: true,
            ..Default::default()
        };
        assert!(prepared("y\n", false, confirm).is_none());
        assert!(prepared("", false, LaunchOptions::default()).is_none());
    }

    #[test]
    fn eof_at_chdir_prompt_aborts() {
        let confirm = LaunchOptions {
            confirm_chdir: true,
            ..Default::default()
        };
        assert!(prepared("", true, confirm).is_none());
        // "n" keeps the cwd and still launches.
        assert!(prepared("n\n", true, confirm).is_some());
    }
}
