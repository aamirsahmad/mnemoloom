mod mux;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::FromArgMatches;
use clap::error::ErrorKind;

use mnemoloom::cli::{self, Cli, Command, HookCommand, MenuArgs};
use mnemoloom::{commands, exit, hook, index, menu, ops, session};

fn main() -> ExitCode {
    #[cfg(unix)]
    // Rust ignores SIGPIPE by default; normal CLI pipelines expect it to terminate.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let args: Vec<OsString> = std::env::args_os().collect();
    let typo = cli::misspelt_subcommand(&args);
    let args = cli::rewrite_bare_session(args);
    let cli = match cli::command()
        .try_get_matches_from(&args)
        .and_then(|m| Cli::from_arg_matches(&m))
    {
        Ok(c) => c,
        Err(e) => return parse_error(&args, e, typo),
    };
    let code = match dispatch(&cli) {
        Ok(code) => return code,
        Err(e) => report(e),
    };
    did_you_mean(typo);
    code
}

/// `mnemoloom serach x`: the bare-session rewrite made a typo a session lookup.
fn did_you_mean(typo: Option<&str>) {
    if let Some(sub) = typo {
        eprintln!("(did you mean 'mnemoloom {sub}'?)");
    }
}

/// JSON contracts for usage errors: `resolve` -> `{"error":"invalid_input"}`,
/// `trim-in-place --json` -> `{"error": msg}`; both exit 1.
fn parse_error(args: &[OsString], e: clap::Error, typo: Option<&str>) -> ExitCode {
    let informational = matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion);
    let sub = args
        .iter()
        .skip(1)
        .filter_map(|a| a.to_str())
        .find(|a| cli::SUBCOMMANDS.contains(a));
    let detail = summary(&e);
    if !informational && sub == Some("resolve") {
        println!("{}", commands::resolve::usage_error_json(&detail));
        return ExitCode::from(1);
    }
    if !informational && sub == Some("trim-in-place") && args.iter().any(|a| a == "--json") {
        println!(
            "{}",
            session::pyjson::dumps(&serde_json::json!({ "error": detail }))
        );
        return ExitCode::from(1);
    }
    if !informational && typo.is_some() {
        let _ = e.print();
        did_you_mean(typo);
        return ExitCode::from(2);
    }
    e.exit()
}

/// Clap's message without `error: ` / usage / tips, on one line.
fn summary(e: &clap::Error) -> String {
    let text = e.to_string();
    text.lines()
        .take_while(|l| !l.trim().is_empty())
        .map(|l| l.trim().trim_start_matches("error: "))
        .collect::<Vec<_>>()
        .join(" ")
}

fn report(e: anyhow::Error) -> ExitCode {
    if let Some(x) = e.downcast_ref::<exit::Exit>() {
        if let Some(m) = &x.message {
            eprintln!("{m}");
        }
        return ExitCode::from(x.code);
    }
    eprintln!("Error: {e:#}");
    ExitCode::from(1)
}

fn dispatch(cli: &Cli) -> anyhow::Result<ExitCode> {
    let g = &cli.global;
    session::homes::validate_home_flags(&g.home_flags())?;
    let Some(cmd) = &cli.command else {
        return menu::menu(g, &MenuArgs::default());
    };
    match cmd {
        Command::BuildIndex(a) => index::build_index(g, a),
        Command::ClearIndex(a) => index::clear_index(g, a),
        Command::IndexStats(a) => index::index_stats(g, a),
        Command::Search(a) => index::search(g, a),
        Command::Resolve(a) => commands::resolve::run(g, a),
        Command::Menu(a) => menu::menu(g, a),
        Command::Resume(a) => commands::resume::run(g, a),
        Command::Clone(a) => ops::clone::clone(g, a),
        Command::Trim(a) => ops::trim::trim(g, a),
        Command::TrimInPlace(a) => ops::trim::trim_in_place(g, a),
        Command::SmartTrim(a) => ops::trim::smart_trim(g, a),
        Command::Rollover(a) => ops::rollover::rollover(g, a),
        Command::Lineage(a) => commands::lineage::lineage(g, a),
        Command::Info(a) => commands::info::run(g, a),
        Command::Copy(a) => commands::files::copy(g, a),
        Command::Move(a) => commands::files::move_session(g, a),
        Command::MoveAccount(a) => commands::move_account::run(g, a),
        Command::Delete(a) => commands::files::delete(g, a),
        Command::Export(a) => ops::export::export(g, a),
        Command::ExportClaude(a) => ops::export::export_claude(g, a).map_err(|e| {
            commands::wrong_agent_export(e, g, a.session.as_deref(), session::Agent::Claude)
        }),
        Command::ExportCodex(a) => ops::export::export_codex(g, a).map_err(|e| {
            commands::wrong_agent_export(e, g, a.session.as_deref(), session::Agent::Codex)
        }),
        Command::Port(a) => ops::port::port(g, a),
        Command::Query(a) => ops::query::query(g, a),
        Command::FindOriginal(a) => commands::lineage::find_original(g, a),
        Command::FindDerived(a) => commands::lineage::find_derived(g, a),
        Command::Hook(HookCommand::Resume(a)) => hook::resume(g, a),
        Command::Mux(a) => Ok(mux::main(a.args.clone())),
    }
}
