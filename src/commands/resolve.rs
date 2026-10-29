//! `mnemoloom resolve QUERY [--agent claude|codex|pi|opencode] [--home PATH] [--json|--pretty]`.
//! Exit: 0 single, 2 ambiguous, 1 otherwise. No interactive prompts.

use std::io::IsTerminal;
use std::process::ExitCode;

use serde_json::json;

use crate::cli::{GlobalArgs, ResolveArgs};
use crate::index::tui::text::width;
use crate::session::Agent;
use crate::session::pyjson;
use crate::session::resolve::{
    HomeResolution, Outcome, ResolverError, SessionRecord, resolve_in_home,
};

pub fn run(global: &GlobalArgs, args: &ResolveArgs) -> anyhow::Result<ExitCode> {
    if args.json && args.pretty {
        println!(
            r#"{{"error":"invalid_format","detail":"Choose only one of --json or --pretty."}}"#
        );
        return Ok(ExitCode::from(1));
    }
    let pretty = args.pretty || (!args.json && std::io::stdout().is_terminal());
    let result = Agent::parse(&args.agent)
        .ok_or_else(|| {
            ResolverError::new(
                "invalid_agent",
                format!("Unsupported agent: {}", args.agent),
            )
        })
        .and_then(|agent| {
            let home = args.home.as_deref().or(match agent {
                Agent::Claude => global.claude_home.as_deref(),
                Agent::Codex => global.codex_home.as_deref(),
                Agent::Pi => global.pi_home.as_deref(),
                Agent::OpenCode => global.opencode_home.as_deref(),
            });
            resolve_in_home(&args.query, agent, home)
        });
    let res = match result {
        Ok(r) => r,
        Err(e) => {
            if pretty {
                println!("Error: {}", e.detail);
            } else {
                println!("{}", pyjson::dumps(&e.to_json()));
            }
            return Ok(ExitCode::from(1));
        }
    };
    if pretty {
        render_pretty(&res);
    } else {
        println!("{}", pyjson::dumps(&res.to_json()));
    }
    Ok(ExitCode::from(match res.outcome {
        Outcome::Single(_) => 0,
        Outcome::Ambiguous { .. } => 2,
        Outcome::NotFound => 1,
    }))
}

/// Usage-error contract: `{"error": "invalid_input", "detail": ...}`.
pub fn usage_error_json(detail: &str) -> String {
    pyjson::dumps(&json!({"error": "invalid_input", "detail": detail}))
}

fn short_id(id: &str) -> String {
    format!("{}...", id.chars().take(8).collect::<String>())
}

fn render_pretty(res: &HomeResolution) {
    match &res.outcome {
        Outcome::Single(r) => print_panel("Session", &single_rows(r)),
        Outcome::Ambiguous {
            records,
            match_count,
        } => {
            println!(
                "{match_count} sessions match '{}' — disambiguate:",
                res.query
            );
            print_table(
                &["Session ID", "Name", "Directory", "Modified", "Archived"],
                &records
                    .iter()
                    .map(|r| {
                        vec![
                            short_id(&r.session_id),
                            r.name.clone().unwrap_or_else(|| "—".into()),
                            r.directory.clone().unwrap_or_else(|| "—".into()),
                            r.modified.clone(),
                            yes_no(r.archived).into(),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
        }
        Outcome::NotFound => println!(
            "No session found for '{}' in {}",
            res.query,
            res.home.display()
        ),
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn single_rows(r: &SessionRecord) -> Vec<(&'static str, String)> {
    vec![
        ("Agent", r.agent.to_string()),
        ("Session ID", r.session_id.clone()),
        ("Name", r.name.clone().unwrap_or_else(|| "—".into())),
        (
            "Directory",
            r.directory.clone().unwrap_or_else(|| "—".into()),
        ),
        ("Home", r.home.display().to_string()),
        ("Session file", r.session_file.display().to_string()),
        (
            "Matched by",
            r.matched_by.map_or("—", |m| m.as_str()).to_string(),
        ),
        ("Modified", r.modified.clone()),
        ("Archived", yes_no(r.archived).into()),
    ]
}

fn print_panel(title: &str, rows: &[(&str, String)]) {
    let label_w = rows.iter().map(|(l, _)| width(l)).max().unwrap_or(0);
    let lines: Vec<String> = rows
        .iter()
        .map(|(l, v)| format!("{l:<label_w$}  {v}"))
        .collect();
    let inner = lines
        .iter()
        .map(|l| width(l))
        .max()
        .unwrap_or(0)
        .max(width(title) + 2)
        + 2;
    let pad = inner - width(title) - 2;
    println!(
        "╭{} {title} {}╮",
        "─".repeat(pad / 2),
        "─".repeat(pad - pad / 2)
    );
    for l in &lines {
        println!("│ {l}{} │", " ".repeat(inner - 2 - width(l)));
    }
    println!("╰{}╯", "─".repeat(inner));
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            rows.iter()
                .map(|r| width(&r[i]))
                .chain([width(h)])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let fmt_row = |cells: Vec<&str>| {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c}{}", " ".repeat(w - width(c))))
            .collect::<Vec<_>>()
            .join("  ")
    };
    println!();
    println!(" {}", fmt_row(headers.to_vec()));
    println!(
        " {}",
        widths
            .iter()
            .map(|w| "━".repeat(*w))
            .collect::<Vec<_>>()
            .join("━━")
    );
    for r in rows {
        println!(" {}", fmt_row(r.iter().map(String::as_str).collect()));
    }
    println!();
}
