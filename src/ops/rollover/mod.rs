//! `rollover`: hand work off to a fresh session
//! created headlessly with the lineage prompt, link it to the parent with
//! `continue_metadata`, then resume it interactively.

use std::ffi::OsStr;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

use super::trim::{set_first_line_metadata, utc_now_iso};
use crate::cli::{GlobalArgs, MenuArgs, RolloverArgs, StartScreen, agent_of};
use crate::exit::Exit;
use crate::ops::headless::{
    self, AgentBins, capture, find_session_file, interrupted_exit, json_lines, last_json_object,
};
use crate::session::homes::user_home;
use crate::session::launch::{command_hint, may_launch, prompt_line};
use crate::session::lineage::{
    Derivation, RolloverKind, full_chain, rollover_prompt, session_file_list,
};
use crate::session::transcript::WriteStyle;
use crate::session::{Agent, Homes, ResolveOptions, Transcript, path_session_uuid, resolve};

/// Models used by rollover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    pub claude_subagent_model: String,
    pub codex_rollover_model: String,
    pub codex_default_model: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            claude_subagent_model: "haiku".into(),
            codex_rollover_model: "gpt-5.1-codex-mini".into(),
            codex_default_model: String::new(),
        }
    }
}

/// One rollover request (after lineage tracing and agent choice).
#[derive(Debug, Clone)]
pub(crate) struct Request<'a> {
    pub source: &'a Path,
    /// Lineage, oldest first (source last).
    pub chain: Vec<(PathBuf, Derivation)>,
    pub target: Agent,
    pub kind: RolloverKind,
    pub custom_prompt: Option<&'a str>,
    pub config: Config,
}

/// Default context-recovery instructions (what `--context` uses).
pub use crate::session::lineage::DEFAULT_CONTEXT_INSTRUCTIONS;

/// `mnemoloom rollover [SESSION] [--quick] [-p PROMPT] [--agent]`.
/// Hidden (menu) flags: `--context` = context rollover with the default
/// instructions; `--target-agent A` = skip the "which agent" question.
/// Without `--quick`, a non-empty `--prompt` or `--context`, the menu opens.
pub fn rollover(global: &GlobalArgs, args: &RolloverArgs) -> anyhow::Result<ExitCode> {
    let custom_prompt = args.prompt.as_deref().filter(|p| !p.is_empty());
    if !args.quick && !args.context && custom_prompt.is_none() {
        return crate::menu::menu(
            global,
            &MenuArgs {
                session: args.session.clone(),
                agent: args.agent,
                start_screen: StartScreen::Lineage,
                ..Default::default()
            },
        );
    }
    let Some(query) = &args.session else {
        return Err(Exit::fail(
            "Error: --quick or --prompt require SESSION\nUse 'mnemoloom rollover' without options for interactive mode",
        )
        .into());
    };
    let homes = global.homes()?;
    let res = resolve(
        query,
        &ResolveOptions::new(&homes).agent(agent_of(args.agent)),
    )?;
    res.agent.require_rewritable("rollover")?;
    if let Some(target) = agent_of(args.target_agent) {
        target.require_rewritable("rollover")?;
    }
    let bins = AgentBins::from_env();
    println!("\n🔄 Starting rollover to fresh session...\n");
    let chain = trace_lineage(&res.session_file);
    let target = match agent_of(args.target_agent) {
        Some(a) => {
            println!("ℹ️  Using specified agent: {}", upper(a));
            Some(a)
        }
        None => choose_target(res.agent, &bins)?,
    };
    let Some(target) = target else {
        println!("\nCancelled.");
        return Ok(ExitCode::SUCCESS);
    };
    println!("\nℹ️  Rolling over with {}", upper(target));
    match custom_prompt {
        Some(p) => println!(
            "ℹ️  Using custom prompt: {}...",
            p.chars().take(50).collect::<String>()
        ),
        None => println!("ℹ️  No custom summarization instructions"),
    }
    let req = Request {
        source: &res.session_file,
        chain,
        target,
        kind: if args.quick {
            RolloverKind::Quick
        } else {
            RolloverKind::Context
        },
        custom_prompt,
        config: Config::default(),
    };
    let args = run(&req, &homes, &bins)?;
    launch(&bins, target, &args, &homes, io::stdin().is_terminal())
}

/// Replace `mnemoloom` with the interactive agent (nothing follows the resume;
/// Ctrl-C belongs to the agent). Without a terminal, print the command
/// instead and succeed.
fn launch(
    bins: &AgentBins,
    target: Agent,
    args: &[String],
    homes: &Homes,
    interactive: bool,
) -> anyhow::Result<ExitCode> {
    if !may_launch(target, interactive, || {
        launch_hint(bins, target, args, homes)
    }) {
        return Ok(ExitCode::SUCCESS);
    }
    let err = headless::exec(bins, target, args, Some(homes));
    Err(Exit::fail(format!("Error: launching {target}: {err}")).into())
}

/// The shell form of the final launch (`[VAR=<home>] <bin> <args>`).
fn launch_hint(bins: &AgentBins, target: Agent, args: &[String], homes: &Homes) -> String {
    let argv: Vec<&OsStr> = std::iter::once(bins.get(target))
        .chain(args.iter().map(OsStr::new))
        .collect();
    command_hint(target, &argv, None, homes)
}

fn upper(a: Agent) -> String {
    a.as_str().to_uppercase()
}

/// Step 1: print the lineage; returns it oldest first.
fn trace_lineage(source: &Path) -> Vec<(PathBuf, Derivation)> {
    println!("Step 1: Tracing session lineage...");
    let mut chain = full_chain(source);
    chain.reverse();
    if chain.len() > 1 {
        println!("✅ Found {} session(s) in lineage:", chain.len());
        println!("{}\n", session_file_list(&chain));
    } else {
        println!("✅ This is the original session (no parent chain)\n");
    }
    chain
}

/// Target agent: the current one unless the other agent is available, in
/// which case the user is asked (piped answers work too).
/// `None` = cancelled (EOF).
fn choose_target(current: Agent, bins: &AgentBins) -> anyhow::Result<Option<Agent>> {
    let other = current.other();
    if !agent_available(other, bins) {
        println!("ℹ️  Rolling over with {}", upper(current));
        return Ok(Some(current));
    }
    println!("Current session is from: {}", upper(current));
    println!("Which agent should take over the work?");
    println!("1. {} (default - same agent)", upper(current));
    println!("2. {} (cross-agent)", upper(other));
    println!();
    let Some(choice) = prompt_line(&format!(
        "Enter choice [1-2] (or Enter for {}): ",
        upper(current)
    ))?
    else {
        return Ok(None);
    };
    Ok(Some(match choice.as_str() {
        "" | "1" => current,
        "2" => other,
        _ => {
            println!("Invalid choice, using default.");
            current
        }
    }))
}

/// Agent available: binary on PATH or `~/.<agent>` exists.
fn agent_available(agent: Agent, bins: &AgentBins) -> bool {
    let bin = Path::new(bins.get(agent));
    let on_path = if bin.components().count() > 1 {
        bin.is_file()
    } else {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
    };
    on_path || user_home().join(format!(".{agent}")).is_dir()
}

/// The prompt for the headless call.
pub(crate) fn build_prompt(req: &Request) -> String {
    let subagents = match req.target {
        Agent::Claude => format!(
            "PARALLEL {} SUB-AGENTS",
            req.config.claude_subagent_model.to_uppercase()
        ),
        Agent::Codex => "parallel sub-agents (if available)".to_string(),
        Agent::Pi | Agent::OpenCode => unreachable!("refused by require_rewritable"),
    };
    rollover_prompt(&req.chain, req.kind, req.custom_prompt, Some(&subagents))
}

/// Steps 2–3 for the chosen target agent: create the session headlessly,
/// link it, print the launch step; returns the resume argv (without the
/// program) for [`headless::exec`].
pub(crate) fn run(req: &Request, homes: &Homes, bins: &AgentBins) -> anyhow::Result<Vec<String>> {
    let prompt = build_prompt(req);
    println!(
        "🔄 {} Rollover - Transferring context to fresh session\n",
        req.target.label()
    );
    println!("ℹ️  Using {} parent session file(s)\n", req.chain.len());
    let quick = req.kind == RolloverKind::Quick;
    if quick {
        println!("Step 2: 🚀 Creating session with lineage info...");
    } else if req.target == Agent::Claude {
        println!("Step 2: 🤖 Creating session and analyzing history with sub-agents...");
        println!("   Analyzing {} session file(s)...", req.chain.len());
    } else {
        println!("Step 2: 🤖 Creating session and analyzing history...");
        println!("   Analyzing {} session file(s)...", req.chain.len());
        println!("   Using model: {}", req.config.codex_rollover_model);
    }
    println!();
    let new_id = match req.target {
        Agent::Claude => create_claude(&prompt, homes, bins)?,
        Agent::Codex => {
            let model = (!quick).then_some(req.config.codex_rollover_model.as_str());
            create_codex(&prompt, model, homes, bins)?
        }
        Agent::Pi | Agent::OpenCode => unreachable!("refused by require_rewritable"),
    };
    println!("✅ Session created and analysis complete: {new_id}\n");
    let parent_id = match req.target {
        Agent::Claude => req
            .source
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        Agent::Codex => path_session_uuid(req.source),
        Agent::Pi | Agent::OpenCode => unreachable!("refused by require_rewritable"),
    };
    // Look the new session file up rather than computing its path from the
    // cwd, so a project-dir mismatch cannot lose the lineage link.
    if let Some(file) = find_session_file(req.target, &new_id, homes) {
        // Metadata injection errors are ignored.
        let _ = add_continue_metadata(&file, req.target, &parent_id, req.source);
    }
    Ok(launch_args(req.target, &new_id, &req.config, homes, bins))
}

/// Headless spawn failure: Ctrl-C -> exit 130, else a creation error message.
fn creation_error(e: std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::Interrupted {
        interrupted_exit().into()
    } else {
        Exit::fail(format!("❌ Error creating session: {e}")).into()
    }
}

/// `claude -p <prompt> --output-format json` -> `.session_id`.
fn create_claude(prompt: &str, homes: &Homes, bins: &AgentBins) -> anyhow::Result<String> {
    println!("$ claude -p '<analysis prompt>' --output-format json");
    let args = ["-p", prompt, "--output-format", "json"].map(String::from);
    let out = capture(bins, Agent::Claude, &args, Some(homes), None).map_err(creation_error)?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(Exit::fail(format!(
            "❌ Error creating session: claude exited with {}\n   stdout: {stdout}\n   stderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ))
        .into());
    }
    claude_session_id(&stdout).ok_or_else(|| {
        Exit::fail(format!(
            "❌ Error creating session: Could not find session_id in output: {stdout}"
        ))
        .into()
    })
}

/// `session_id` of the last JSON object line (shell-fallback noise skipped).
fn claude_session_id(stdout: &str) -> Option<String> {
    last_json_object(stdout)?
        .get("session_id")?
        .as_str()
        .map(str::to_string)
}

/// `codex exec --json [--model M] <prompt>` -> `thread.started.thread_id`.
fn create_codex(
    prompt: &str,
    model: Option<&str>,
    homes: &Homes,
    bins: &AgentBins,
) -> anyhow::Result<String> {
    let mut args = vec!["exec".to_string(), "--json".to_string()];
    if let Some(m) = model {
        args.extend(["--model".to_string(), m.to_string()]);
    }
    args.push(prompt.to_string());
    let out = capture(bins, Agent::Codex, &args, Some(homes), None).map_err(creation_error)?;
    if !out.status.success() {
        return Err(Exit::fail(format!(
            "❌ Error creating session: codex exited with {}\n   {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ))
        .into());
    }
    codex_thread_id(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| {
        Exit::fail("❌ Error: Could not extract thread_id from codex exec output").into()
    })
}

/// `thread_id` of the first `{"type":"thread.started"}` event.
pub(crate) fn codex_thread_id(stdout: &str) -> Option<String> {
    json_lines(stdout)
        .find(|v| v.get("type").and_then(Value::as_str) == Some("thread.started"))
        .and_then(|v| v.get("thread_id")?.as_str().map(str::to_string))
}

/// First line: drop `trim_metadata`, set `continue_metadata`.
pub(crate) fn add_continue_metadata(
    file: &Path,
    agent: Agent,
    parent_id: &str,
    parent_file: &Path,
) -> anyhow::Result<()> {
    let mut t = Transcript::load(file, agent)?;
    let meta = json!({
        "parent_session_id": parent_id,
        "parent_session_file": crate::session::absolute(parent_file).display().to_string(),
        "continued_at": utc_now_iso(),
    });
    if set_first_line_metadata(
        &mut t.lines,
        Some("trim_metadata"),
        "continue_metadata",
        meta,
    ) {
        t.write_atomic(file, WriteStyle::Python, None)?;
    }
    Ok(())
}

/// Step 3: print the launch and return the resume argv.
fn launch_args(
    agent: Agent,
    id: &str,
    config: &Config,
    homes: &Homes,
    bins: &AgentBins,
) -> Vec<String> {
    let mut args = match agent {
        Agent::Claude => vec!["--resume".to_string(), id.to_string()],
        Agent::Codex => vec!["resume".to_string(), id.to_string()],
        Agent::Pi | Agent::OpenCode => unreachable!("refused by require_rewritable"),
    };
    println!(
        "Step 3: 🚀 Launching interactive {} session...",
        match agent {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Pi | Agent::OpenCode => unreachable!("refused by require_rewritable"),
        }
    );
    println!("   Session ID: {id}");
    let model = &config.codex_default_model;
    match agent {
        Agent::Claude | Agent::Pi | Agent::OpenCode => {}
        Agent::Codex if model.is_empty() => println!("   (Using codex default model)"),
        Agent::Codex => {
            println!("   Model: {model}");
            args.extend(["-c".to_string(), format!("model={model}")]);
        }
    }
    println!("$ {}", launch_hint(bins, agent, &args, homes));
    println!("\n{}\n", "=".repeat(70));
    args
}

#[cfg(test)]
mod tests;
