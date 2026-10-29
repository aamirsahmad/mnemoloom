//! `mnemoloom lineage`, `mnemoloom find-original`, `mnemoloom find-derived`.

use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::{Value, json};

use super::{file_name, latest_session_with, thousands};
use crate::cli::{FindDerivedArgs, FindOriginalArgs, GlobalArgs, LineageArgs, agent_of};
use crate::session::lineage::{
    Derivation, child_index, descendants, full_chain, parent_info, read_first_line,
};
use crate::session::timefmt::iso_local_naive;
use crate::session::{
    Agent, Depth, ResolveOptions, SessionFile, opencode, path_session_uuid, pyjson, resolve,
    session_stem,
};

/// `mnemoloom lineage [SESSION] [--agent] [--json]` (no SESSION = latest session).
pub fn lineage(global: &GlobalArgs, args: &LineageArgs) -> anyhow::Result<ExitCode> {
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
    let chain = full_chain(&res.session_file);
    if chain.len() == 1 && chain[0].1 == Derivation::Original {
        if args.json {
            println!("[]");
        } else {
            println!("No lineage found (this is an original session).");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let nodes: Vec<Value> = chain
        .iter()
        .map(|(p, d)| {
            let modified = modified_of(p, res.agent)
                .map(iso_local_naive)
                .unwrap_or_default();
            json!({
                "session_id": path_session_uuid(p),
                "file_path": p.to_string_lossy(),
                "derivation_type": d.as_str(),
                "modified": modified,
            })
        })
        .collect();
    if args.json {
        println!("{}", pyjson::dumps_indent(&Value::Array(nodes), 2));
        return Ok(ExitCode::SUCCESS);
    }
    let stem = session_stem(&res.session_file);
    println!("\nLineage for: {stem}");
    println!("{}", "=".repeat(60));
    println!("Chain has {} session(s):\n", nodes.len());
    for (i, ((p, d), node)) in chain.iter().zip(&nodes).enumerate() {
        let marker = if i == 0 { "► " } else { "  " };
        let branch = if i + 1 == nodes.len() {
            "└─"
        } else {
            "├─"
        };
        let modified: String = node["modified"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(10)
            .collect();
        println!(
            "{marker}{branch} [{:<10}] {}  ({modified})",
            d.as_str(),
            file_name(p)
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// mtime of a chain member (opencode: the session's `time_updated`).
fn modified_of(p: &std::path::Path, agent: Agent) -> Option<std::time::SystemTime> {
    if agent == Agent::OpenCode {
        return SessionFile::read(p, agent, Depth::Header)
            .ok()
            .map(|s| s.modified);
    }
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// `mnemoloom find-original SESSION [-v]`: print the chain root's path.
pub fn find_original(global: &GlobalArgs, args: &FindOriginalArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    let res = resolve(
        &args.session,
        &ResolveOptions::new(&homes).prompt("Which session's original do you want to find?"),
    )?;
    let chain = full_chain(&res.session_file);
    if args.verbose {
        eprintln!("Following parent links...");
        eprintln!("\nParent chain:");
        for (i, (p, d)) in chain.iter().enumerate() {
            let indent = "  ".repeat(i);
            let arrow = if i > 0 { "└─> " } else { "" };
            let label = if *d == Derivation::Original {
                String::new()
            } else {
                format!(" ({})", d.as_str())
            };
            eprintln!("{indent}{arrow}{}{label}", p.display());
            if *d == Derivation::Continued
                && let Some(exported) = parent_info(p).and_then(|l| l.exported_chat_log)
            {
                let exported = if exported.is_absolute() {
                    exported
                } else {
                    let abs = p.parent().unwrap_or(p).join(&exported);
                    if abs.exists() { abs } else { exported }
                };
                eprintln!("{indent}    Exported chat: {}", exported.display());
            }
        }
        eprintln!();
    }
    let root = chain
        .last()
        .map(|(p, _)| p.clone())
        .unwrap_or(res.session_file);
    println!("{}", root.display());
    Ok(ExitCode::SUCCESS)
}

/// `mnemoloom find-derived SESSION [-t] [-d DIR] [-s]`. Rollover
/// (`continue_metadata`) children are found too.
pub fn find_derived(global: &GlobalArgs, args: &FindDerivedArgs) -> anyhow::Result<ExitCode> {
    let homes = global.homes()?;
    let res = resolve(
        &args.session,
        &ResolveOptions::new(&homes).prompt("Which session's descendants do you want to find?"),
    )?;
    let root = res.session_file;
    let dirs: Vec<PathBuf> = match &args.search_dir {
        Some(d) => vec![d.clone()],
        None => vec![homes.claude_projects(), homes.codex_sessions()],
    };
    let index = if res.agent == Agent::OpenCode {
        opencode::child_index(&homes.opencode)
    } else {
        child_index(&dirs)
    };
    let tree = descendants(&root, &index);
    let mut all: Vec<PathBuf> = tree
        .iter()
        .flat_map(|(_, kids)| kids.iter().map(|(p, _)| p.clone()))
        .collect();
    if all.is_empty() {
        println!("No trimmed sessions found for: {}", root.display());
        return Ok(ExitCode::SUCCESS);
    }
    all.sort();
    if args.tree {
        println!("{}", root.display());
        print_tree(&tree, &root, "");
    } else {
        for p in &all {
            println!("{}", p.display());
        }
    }
    if args.stats {
        println!("\n{}", "=".repeat(70));
        println!("TRIM STATISTICS");
        println!("{}", "=".repeat(70));
        for p in &all {
            let Some(tm) = read_first_line(p).and_then(|v| v.get("trim_metadata").cloned()) else {
                continue;
            };
            let stat = |k: &str| {
                tm.pointer(&format!("/stats/{k}"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            };
            let params = tm.get("trim_params").cloned().unwrap_or(Value::Null);
            println!("\nFile: {}", p.display());
            println!("  Tools trimmed: {}", stat("num_tools_trimmed"));
            println!(
                "  Assistant msgs trimmed: {}",
                stat("num_assistant_trimmed")
            );
            println!("  Tokens saved: {}", thousands(stat("tokens_saved")));
            let threshold = params
                .get("threshold")
                .map_or("N/A".to_string(), |t| t.to_string());
            println!("  Threshold: {threshold}");
            if let Some(tools) = params
                .get("tools")
                .and_then(Value::as_array)
                .filter(|t| !t.is_empty())
            {
                let names: Vec<&str> = tools.iter().filter_map(Value::as_str).collect();
                println!("  Tools: {}", names.join(", "));
            }
            if let Some(mode) = params
                .get("trim_assistant_messages")
                .filter(|m| !m.is_null() && m.as_i64() != Some(0))
            {
                println!("  Assistant trim mode: {mode}");
            }
        }
        println!("{}", "=".repeat(70));
    }
    Ok(ExitCode::SUCCESS)
}

fn print_tree(tree: &[(PathBuf, Vec<(PathBuf, Derivation)>)], node: &PathBuf, indent: &str) {
    let Some((_, kids)) = tree.iter().find(|(p, _)| p == node) else {
        return;
    };
    for (i, (child, _)) in kids.iter().enumerate() {
        let last = i + 1 == kids.len();
        println!(
            "{indent}{}{}",
            if last { "└─> " } else { "├─> " },
            child.display()
        );
        print_tree(
            tree,
            child,
            &format!("{indent}{}", if last { "    " } else { "│   " }),
        );
    }
}
