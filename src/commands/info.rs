//! `mnemoloom info [SESSION] [--agent] [--json]`.
//!
//! `Title` is filled from the session name; with no SESSION the full info
//! of the latest project session is shown.

use std::process::ExitCode;

use serde_json::json;

use super::{file_name, latest_session_with, thousands};
use crate::cli::{GlobalArgs, InfoArgs, agent_of};
use crate::session::discover::codex_thread_names;
use crate::session::lineage::continuation_lineage;
use crate::session::meta::count_user_messages;
use crate::session::timefmt::{iso_local_naive, local_display};
use crate::session::{Agent, Depth, ResolveOptions, pyjson, resolve};

pub fn run(global: &GlobalArgs, args: &InfoArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    let agent = agent_of(args.agent);
    let res = match &args.session {
        Some(q) => resolve(
            q,
            &ResolveOptions::new(&homes)
                .agent(agent)
                .interactive(!args.json),
        )?,
        None => latest_session_with(
            &homes,
            agent,
            "Which session do you want to use?",
            !args.json,
        )?,
    };
    let mut sf = res.metadata(Depth::Full)?;
    if sf.agent == Agent::Codex {
        sf = sf.with_codex_name(&codex_thread_names(&homes.codex));
    }
    let cwd = sf.cwd.clone().or(res.cwd.clone());
    let title = sf.name.clone().unwrap_or_default();
    let user_msgs = count_user_messages(&sf.path, sf.agent);
    let lineage: Vec<_> = continuation_lineage(&sf.path)
        .into_iter()
        .map(|p| json!({"file": p.to_string_lossy(), "type": "continued"}))
        .collect();

    if args.json {
        let v = json!({
            "session_id": sf.session_id,
            "agent": sf.agent.as_str(),
            "custom_title": title,
            "file_path": sf.path.to_string_lossy(),
            "project": sf.project(),
            "cwd": cwd,
            "created": iso_local_naive(sf.changed),
            "modified": iso_local_naive(sf.modified),
            "file_size_bytes": sf.size,
            "line_count": sf.line_count.unwrap_or(0),
            "user_message_count": user_msgs,
            "export_path": sf.default_export_path().to_string_lossy(),
            "lineage": lineage,
        });
        println!("{}", pyjson::dumps_indent(&v, 2));
        return Ok(ExitCode::SUCCESS);
    }

    let rule = "=".repeat(60);
    println!("\n{rule}");
    println!("Session: {}", sf.session_id);
    if !title.is_empty() {
        println!("Title:   {title}");
    }
    println!("{rule}");
    println!("Agent:      {}", sf.agent);
    println!("Project:    {}", sf.project());
    println!("CWD:        {}", cwd.as_deref().unwrap_or("None"));
    println!("File:       {}", sf.path.display());
    println!("Created:    {}", local_display(sf.changed));
    println!("Modified:   {}", local_display(sf.modified));
    println!("Size:       {} bytes", thousands(sf.size));
    println!(
        "Lines:      {}",
        thousands(sf.line_count.unwrap_or(0) as u64)
    );
    println!("User msgs:  {user_msgs}");
    if !lineage.is_empty() {
        println!("\nLineage ({} sessions):", lineage.len());
        for (i, node) in lineage.iter().enumerate() {
            let prefix = if i + 1 == lineage.len() {
                "  └─"
            } else {
                "  ├─"
            };
            let f = node["file"].as_str().unwrap_or_default();
            println!(
                "{prefix} {} (continued)",
                file_name(std::path::Path::new(f))
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}
