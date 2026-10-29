//! `mnemoloom resume [SESSION] [--agent]`: plain resume (`claude -r` / `codex
//! resume` / `pi --session` / `opencode -s`) in the session's cwd. The resume submenu (clone / trim /
//! smart-trim / rollover) lives in `crate::menu` (start screen `resume`).

use std::process::ExitCode;

use super::latest_session;
use crate::cli::{GlobalArgs, ResumeArgs, agent_of};
use crate::session::launch::{LaunchOptions, exec_resume};
use crate::session::{ResolveOptions, resolve};

pub fn run(global: &GlobalArgs, args: &ResumeArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    let agent = agent_of(args.agent);
    let res = match &args.session {
        Some(q) => resolve(q, &ResolveOptions::new(&homes).agent(agent))?,
        None => latest_session(&homes, agent, "Which session to resume?")?,
    };
    if res.agent == crate::session::Agent::Claude
        && res
            .metadata(crate::session::Depth::Header)
            .is_ok_and(|m| m.is_sidechain)
    {
        anyhow::bail!(
            "Sub-agent session (not resumable): {}",
            res.session_file.display()
        );
    }
    exec_resume(
        res.agent,
        &res.resume_target(),
        res.cwd.as_deref(),
        &homes,
        LaunchOptions {
            confirm_chdir: true,
            ..Default::default()
        },
    )?;
    Ok(ExitCode::SUCCESS)
}
