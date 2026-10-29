//! Command-line definitions for `mnemoloom`.
//!
//! The home flags (`--claude-home`, `--codex-home`, `--pi-home`,
//! `--opencode-home`) are global: accepted before or after the subcommand.
//! Bare `mnemoloom <session>` is rewritten to `mnemoloom menu <session>` by
//! [`rewrite_bare_session`]; bare `mnemoloom` parses to no subcommand (= menu
//! for the latest session).

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};

use crate::session::{Agent, Homes};

/// Session tools for Claude Code, Codex, pi and opencode.
#[derive(Parser, Debug)]
#[command(
    name = "mnemoloom",
    version,
    about = "Session management tools for Claude Code, Codex, pi and opencode.",
    after_help = "Environment variables:\n  CLAUDE_CONFIG_DIR    Default Claude home (overridden by --claude-home)\n  CODEX_HOME           Default Codex home (overridden by --codex-home)\n  PI_CODING_AGENT_DIR  Default pi home (overridden by --pi-home)\n  XDG_DATA_HOME        opencode data dir parent (overridden by --opencode-home)"
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Options valid anywhere on the command line (listed under their own
/// heading, after each command's options).
#[derive(Args, Debug, Clone, Default)]
#[command(next_help_heading = "Homes")]
pub struct GlobalArgs {
    /// Claude home directory (default: ~/.claude or CLAUDE_CONFIG_DIR)
    #[arg(long, global = true, value_name = "DIR")]
    pub claude_home: Option<PathBuf>,
    /// Codex home directory (default: ~/.codex or CODEX_HOME)
    #[arg(long, global = true, value_name = "DIR")]
    pub codex_home: Option<PathBuf>,
    /// pi agent directory (default: ~/.pi/agent or PI_CODING_AGENT_DIR)
    #[arg(long, global = true, value_name = "DIR")]
    pub pi_home: Option<PathBuf>,
    /// opencode data directory (default: $XDG_DATA_HOME/opencode or ~/.local/share/opencode)
    #[arg(long, global = true, value_name = "DIR")]
    pub opencode_home: Option<PathBuf>,
}

impl GlobalArgs {
    /// Resolve every agent home (flag > env > default).
    pub fn homes(&self) -> anyhow::Result<Homes> {
        Homes::resolve_all(
            self.claude_home.as_deref(),
            self.codex_home.as_deref(),
            self.pi_home.as_deref(),
            self.opencode_home.as_deref(),
        )
    }

    /// `(flag, value)` for each home flag ([`crate::session::homes::validate_home_flags`]).
    pub fn home_flags(&self) -> [(&'static str, Option<&std::path::Path>); 4] {
        [
            ("--claude-home", self.claude_home.as_deref()),
            ("--codex-home", self.codex_home.as_deref()),
            ("--pi-home", self.pi_home.as_deref()),
            ("--opencode-home", self.opencode_home.as_deref()),
        ]
    }
}

/// `--agent claude|codex|pi|opencode`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentArg {
    Claude,
    Codex,
    Pi,
    #[value(name = "opencode")]
    OpenCode,
}

impl From<AgentArg> for Agent {
    fn from(a: AgentArg) -> Agent {
        match a {
            AgentArg::Claude => Agent::Claude,
            AgentArg::Codex => Agent::Codex,
            AgentArg::Pi => Agent::Pi,
            AgentArg::OpenCode => Agent::OpenCode,
        }
    }
}

/// Convert an optional `--agent`.
pub fn agent_of(a: Option<AgentArg>) -> Option<Agent> {
    a.map(Agent::from)
}

/// `search --agent claude|codex|pi|opencode|all`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchAgent {
    Claude,
    Codex,
    Pi,
    #[value(name = "opencode")]
    OpenCode,
    #[default]
    All,
}

/// Menu start screens (`--start-screen`).
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[value(rename_all = "snake_case")]
pub enum StartScreen {
    #[default]
    Action,
    Resume,
    TrimMenu,
    SmartTrimForm,
    Lineage,
    Query,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Build/update the search index from JSONL session files.
    BuildIndex(BuildIndexArgs),
    /// Clear the search index.
    ClearIndex(ClearIndexArgs),
    /// Show statistics about the search index vs files on disk.
    IndexStats(IndexStatsArgs),
    /// Full-text session search (TUI, or JSONL with --json).
    Search(SearchArgs),
    /// Resolve a session name, full ID, ID fragment, or filename fragment.
    Resolve(ResolveArgs),
    /// Open the action menu for a session name, ID/fragment, or path.
    Menu(MenuArgs),
    /// Resume a session (latest for this project if SESSION is omitted).
    Resume(ResumeArgs),
    /// Clone a session and resume the clone.
    Clone(CloneArgs),
    /// Trim session to reduce size by truncating large tool results.
    Trim(TrimArgs),
    /// Trim a session file IN PLACE (keeps the same session ID).
    TrimInPlace(TrimInPlaceArgs),
    /// Smart trim using headless agents (EXPERIMENTAL).
    SmartTrim(SmartTrimArgs),
    /// Rollover: hand off work to a fresh session with lineage.
    Rollover(RolloverArgs),
    /// Show the parent lineage chain of a session.
    Lineage(LineageArgs),
    /// Show information about a session.
    Info(InfoArgs),
    /// Copy a session file to a new location.
    Copy(CopyArgs),
    /// Move a session to a different project directory.
    Move(MoveArgs),
    /// Move a session to a different account (config dir).
    MoveAccount(MoveAccountArgs),
    /// Delete a Claude or Codex session safely.
    Delete(DeleteArgs),
    /// Export session to text format.
    Export(ExportArgs),
    /// Export a Claude Code session to text format.
    ExportClaude(ExportAgentArgs),
    /// Export a Codex session to text format.
    ExportCodex(ExportAgentArgs),
    /// Port a session to the OTHER agent (codex <-> claude).
    Port(PortArgs),
    /// Query a session with a question using an AI agent.
    Query(QueryArgs),
    /// Find the original session of a trimmed / rolled-over session.
    FindOriginal(FindOriginalArgs),
    /// Find sessions derived (trimmed or rolled over) from a session.
    FindDerived(FindDerivedArgs),
    /// Plugin hook entry points (read hook JSON on stdin).
    #[command(subcommand, hide = true)]
    Hook(HookCommand),
    /// See and jump between coding agents in tmux panes (`mnemoloom mux --help`).
    #[command(disable_help_flag = true)]
    Mux(MuxArgs),
}

/// Subcommand names (for the bare-session rewrite).
pub const SUBCOMMANDS: &[&str] = &[
    "build-index",
    "clear-index",
    "index-stats",
    "search",
    "resolve",
    "menu",
    "resume",
    "clone",
    "trim",
    "trim-in-place",
    "smart-trim",
    "rollover",
    "lineage",
    "info",
    "copy",
    "move",
    "move-account",
    "delete",
    "export",
    "export-claude",
    "export-codex",
    "port",
    "query",
    "find-original",
    "find-derived",
    "hook",
    "mux",
    "help",
];

#[derive(Args, Debug, Clone, Default)]
pub struct BuildIndexArgs {
    /// Index directory (default: $MNEMOLOOM_INDEX_DIR, else <cache>/search-index; cache = $MNEMOLOOM_CACHE_DIR, $XDG_CACHE_HOME/mnemoloom or ~/.cache/mnemoloom)
    #[arg(short = 'i', long = "index", value_name = "PATH")]
    pub index: Option<PathBuf>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ClearIndexArgs {
    /// Index directory (default: $MNEMOLOOM_INDEX_DIR, else <cache>/search-index; cache = $MNEMOLOOM_CACHE_DIR, $XDG_CACHE_HOME/mnemoloom or ~/.cache/mnemoloom)
    #[arg(short = 'i', long = "index", value_name = "PATH")]
    pub index: Option<PathBuf>,
    /// Show what would be deleted
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct IndexStatsArgs {
    /// Index directory (default: $MNEMOLOOM_INDEX_DIR, else <cache>/search-index; cache = $MNEMOLOOM_CACHE_DIR, $XDG_CACHE_HOME/mnemoloom or ~/.cache/mnemoloom)
    #[arg(short = 'i', long = "index", value_name = "PATH")]
    pub index: Option<PathBuf>,
    /// Filter to specific cwd path
    #[arg(short = 'c', long)]
    pub cwd: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct SearchArgs {
    /// Keyword query
    pub query: Option<String>,
    /// Search across all projects (not just current)
    #[arg(short = 'g', long = "global")]
    pub global: bool,
    /// Filter to directory[:branch] (overrides -g)
    #[arg(long)]
    pub dir: Option<String>,
    /// Filter to specific git branch (only effective when not global)
    #[arg(long)]
    pub branch: Option<String>,
    /// Limit number of results displayed
    #[arg(short = 'n', long)]
    pub num_results: Option<usize>,
    /// Exclude original sessions
    #[arg(long)]
    pub no_original: bool,
    /// Include sub-agent sessions (additive)
    #[arg(long)]
    pub sub_agent: bool,
    /// Include headless `codex exec` runs (additive)
    #[arg(long)]
    pub exec_runs: bool,
    /// Exclude trimmed sessions
    #[arg(long)]
    pub no_trimmed: bool,
    /// Exclude rollover sessions
    #[arg(long)]
    pub no_rollover: bool,
    /// Show only currently running sessions
    #[arg(long)]
    pub live: bool,
    /// Only show sessions with at least N user messages
    #[arg(long, value_name = "N")]
    pub min_lines: Option<usize>,
    /// Sessions modified on/after date (YYYYMMDD, YYYY-MM-DD, MM/DD/YY, MM/DD; optional
    /// time suffix like 2026-10-01T16:45)
    #[arg(long, value_name = "DATE")]
    pub after: Option<String>,
    /// Sessions modified on/before date (same formats as --after)
    #[arg(long, value_name = "DATE")]
    pub before: Option<String>,
    /// Filter by agent type
    #[arg(long, value_enum, default_value_t = SearchAgent::All, ignore_case = true)]
    pub agent: SearchAgent,
    /// Output as JSONL for AI agents. Fields per line: session_id, agent, project, branch, cwd,
    /// lines, created, modified, first_msg, last_msg, file_path, derivation_type, is_sidechain,
    /// is_exec_run, custom_title, snippet (plain text, null without a query)
    #[arg(long)]
    pub json: bool,
    /// Sort results by last-modified time (default: sort by relevance)
    #[arg(long)]
    pub by_time: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ResolveArgs {
    /// Session name, full ID, ID fragment, or filename fragment
    pub query: String,
    /// claude|codex|pi|opencode (single agent only)
    #[arg(
        long,
        value_name = "claude|codex|pi|opencode",
        default_value = "claude"
    )]
    pub agent: String,
    /// Home directory of that agent to search.
    #[arg(long, value_name = "PATH")]
    pub home: Option<PathBuf>,
    /// Force JSON output.
    #[arg(long)]
    pub json: bool,
    /// Force human-readable output.
    #[arg(long)]
    pub pretty: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct MenuArgs {
    /// Session name, ID/fragment, filename fragment, or path (default: latest)
    pub session: Option<String>,
    /// Restrict the search to this agent's home
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
    /// Screen to open
    #[arg(long, value_enum, default_value_t = StartScreen::Action)]
    pub start_screen: StartScreen,
    /// Numbered text menu instead of the TUI
    #[arg(long)]
    pub simple_ui: bool,
    /// Print shell commands (cd + resume) instead of executing them
    #[arg(long)]
    pub shell: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ResumeArgs {
    /// Session name, ID/fragment, filename fragment, or path (default: latest)
    pub session: Option<String>,
    /// Restrict the search to this agent's home
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct CloneArgs {
    /// Session name, ID/fragment, filename fragment, or path (default: latest)
    pub session: Option<String>,
    /// Restrict the search to this agent's home
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
}

#[derive(Args, Debug, Clone)]
pub struct TrimArgs {
    /// Session name, ID/fragment, filename fragment, or path (default: latest)
    pub session: Option<String>,
    /// Comma-separated tools to trim (e.g., 'bash,read,edit'). Default: all tools.
    #[arg(short = 't', long)]
    pub tools: Option<String>,
    /// Minimum length threshold in chars for trimming
    #[arg(short = 'l', long = "len", default_value_t = 500)]
    pub threshold: usize,
    /// Trim assistant messages: positive N trims first N over threshold, negative N trims all
    /// except last |N| over threshold
    #[arg(short = 'a', long, allow_negative_numbers = true)]
    pub trim_assistant: Option<i64>,
    /// Output directory (default: same as input file)
    #[arg(short = 'o', long)]
    pub output_dir: Option<PathBuf>,
    /// Force agent type (auto-detected if not specified)
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
    /// Use simple CLI trim instead of the TUI (requires SESSION)
    #[arg(long)]
    pub simple_ui: bool,
}

#[derive(Args, Debug, Clone)]
pub struct TrimInPlaceArgs {
    /// Session name, ID/fragment, filename fragment, or path
    pub session: String,
    /// Comma-separated tools to trim (e.g., 'bash,read,edit'). Default: all tools.
    #[arg(short = 't', long)]
    pub tools: Option<String>,
    /// Minimum length threshold in chars for trimming
    #[arg(short = 'l', long = "len", default_value_t = 500, value_parser = clap::value_parser!(u64).range(1..))]
    pub threshold: u64,
    /// Trim assistant messages: positive N trims first N over threshold, negative N trims all
    /// except last |N| over threshold
    #[arg(short = 'a', long, allow_negative_numbers = true)]
    pub trim_assistant: Option<i64>,
    /// Compute savings without modifying the session file
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Print a single-line JSON result (for scripts/hooks)
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct SmartTrimArgs {
    /// Session name, ID/fragment, filename fragment, or path (default: latest)
    pub session: Option<String>,
    /// Custom instructions for what to prioritize when trimming
    #[arg(short = 'i', long)]
    pub instructions: Option<String>,
    /// Comma-separated message types to never trim (default: user)
    #[arg(short = 'e', long)]
    pub exclude_types: Option<String>,
    /// Always preserve last N messages
    #[arg(short = 'p', long, default_value_t = 10)]
    pub preserve_recent: usize,
    /// Minimum chars for content extraction
    #[arg(short = 'l', long = "len", default_value_t = 200)]
    pub content_threshold: usize,
    /// Output directory (default: same as input)
    #[arg(short = 'o', long)]
    pub output_dir: Option<PathBuf>,
    /// Show what would be trimmed without doing it (still queries the agent)
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Restrict the search to this agent's home
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct RolloverArgs {
    /// Session name, ID/fragment, filename fragment, or path (default: latest)
    pub session: Option<String>,
    /// Quick rollover without context extraction (just preserve lineage)
    #[arg(long)]
    pub quick: bool,
    /// Custom prompt for context extraction
    #[arg(short = 'p', long)]
    pub prompt: Option<String>,
    /// Context rollover with the default recovery instructions (no menu)
    #[arg(long, hide = true)]
    pub context: bool,
    /// Agent that takes over the work (skips the "which agent" prompt)
    #[arg(long, value_enum, ignore_case = true)]
    pub target_agent: Option<AgentArg>,
    /// Restrict the search to this agent's home
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct LineageArgs {
    /// Session name, ID/fragment, filename fragment, or path (default: latest)
    pub session: Option<String>,
    /// Restrict the search to this agent's home
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct InfoArgs {
    /// Session name, ID/fragment, filename fragment, or path (default: latest)
    pub session: Option<String>,
    /// Restrict the search to this agent's home
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct CopyArgs {
    /// Session name, ID/fragment, filename fragment, or path (default: latest)
    pub session: Option<String>,
    /// Destination path (default: prompted)
    #[arg(short = 'd', long)]
    pub dest: Option<String>,
    /// Restrict the search to this agent's home
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
}

#[derive(Args, Debug, Clone)]
pub struct MoveArgs {
    /// Session name, ID/fragment, filename fragment, or path
    pub session: String,
    /// New project directory for the session
    pub new_project: PathBuf,
    /// Restrict the search to this agent's home
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
}

#[derive(Args, Debug, Clone)]
pub struct MoveAccountArgs {
    /// Session name, ID/fragment, filename fragment, or path
    pub session: String,
    /// Target config dir (account), e.g. ~/.claude or ~/.codex
    #[arg(long = "to")]
    pub to: PathBuf,
    /// Source config dir (default: search all local homes — env-var home, ~/.claude / ~/.codex,
    /// and ~/.claude-* / ~/.codex-* siblings)
    #[arg(long = "from")]
    pub from: Option<PathBuf>,
    /// Agent type (auto-detected from the config dirs if not specified)
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
    /// Copy instead of move (leave source account untouched)
    #[arg(long)]
    pub keep: bool,
}

#[derive(Args, Debug, Clone)]
pub struct DeleteArgs {
    /// Session name, ID/fragment, filename fragment, or path
    pub session: String,
    /// Restrict the search to this agent's home
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
    /// Skip the final deletion confirmation
    #[arg(short = 'f', long)]
    pub force: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ExportArgs {
    /// Session name, ID/fragment, filename fragment, or path (default: latest)
    pub session: Option<String>,
    /// Restrict the search to this agent's home
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
    /// Output path (default: <session cwd>/exported-sessions/<agent>/<stem>.txt)
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
    /// Verbose output
    #[arg(short = 'v', long)]
    pub verbose: bool,
}

/// `export-claude` / `export-codex` (agent fixed by the subcommand).
#[derive(Args, Debug, Clone, Default)]
pub struct ExportAgentArgs {
    /// Session name, ID/fragment, filename fragment, or path (default: latest)
    pub session: Option<String>,
    /// Output path (default: <session cwd>/exported-sessions/<agent>/<stem>.txt)
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
    /// Verbose output
    #[arg(short = 'v', long)]
    pub verbose: bool,
}

#[derive(Args, Debug, Clone)]
pub struct PortArgs {
    /// Session name, ID/fragment, filename fragment, or path
    pub session: String,
}

#[derive(Args, Debug, Clone, Default)]
pub struct QueryArgs {
    /// Session name, ID/fragment, filename fragment, or path (default: latest)
    pub session: Option<String>,
    /// Question to ask about the session (default: prompted)
    pub question: Option<String>,
    /// Restrict the search to this agent's home
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<AgentArg>,
}

#[derive(Args, Debug, Clone)]
pub struct FindOriginalArgs {
    /// Session name, ID/fragment, filename fragment, or path
    pub session: String,
    /// Show parent chain traversal
    #[arg(short = 'v', long)]
    pub verbose: bool,
}

#[derive(Args, Debug, Clone)]
pub struct FindDerivedArgs {
    /// Session name, ID/fragment, filename fragment, or path
    pub session: String,
    /// Display results as a tree
    #[arg(short = 't', long)]
    pub tree: bool,
    /// Custom directory to search for derived sessions
    #[arg(short = 'd', long)]
    pub search_dir: Option<PathBuf>,
    /// Show statistics from trim_metadata
    #[arg(short = 's', long)]
    pub stats: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum HookCommand {
    /// UserPromptSubmit hook: `>resume`, `>session`, `>trim` triggers.
    Resume(HookArgs),
}

#[derive(Args, Debug, Clone, Default)]
pub struct HookArgs {}

/// `mnemoloom mux ARGS...`: everything after `mux` is passed through unparsed.
#[derive(Args, Debug, Clone, Default)]
pub struct MuxArgs {
    /// mux arguments (see `mnemoloom mux --help`)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<OsString>,
}

/// [`Cli::command`] with the global home flags hidden under `hook` (internal)
/// and `mux` (which ignores them). Hidden copies are added before clap
/// propagates the globals, which then skips them.
pub fn command() -> clap::Command {
    let cmd = Cli::command();
    let hidden: Vec<clap::Arg> = cmd
        .get_arguments()
        .filter(|a| a.is_global_set())
        .map(|a| a.clone().hide(true))
        .collect();
    cmd.mut_subcommand("hook", |h| h.args(hidden.clone()))
        .mut_subcommand("mux", |m| m.args(hidden))
}

/// Index of the first positional, skipping the values of the home flags
/// (`--claude-home`, ...). `args[0]` is the program name.
fn first_positional(args: &[OsString]) -> Option<usize> {
    const HOME_FLAGS: [&str; 4] = [
        "--claude-home",
        "--codex-home",
        "--pi-home",
        "--opencode-home",
    ];
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if HOME_FLAGS.iter().any(|f| a == f) {
            i += 2;
        } else if a == "--" {
            return None;
        } else if a.as_encoded_bytes().starts_with(b"-") {
            i += 1;
        } else {
            return Some(i);
        }
    }
    None
}

fn is_subcommand(a: &OsString) -> bool {
    a.to_str().is_some_and(|s| SUBCOMMANDS.contains(&s))
}

/// Bare-session shortcut: if the first positional (see
/// [`first_positional`]) is not a subcommand, insert `menu` before it.
pub fn rewrite_bare_session(mut args: Vec<OsString>) -> Vec<OsString> {
    if let Some(i) = first_positional(&args)
        && !is_subcommand(&args[i])
    {
        args.insert(i, "menu".into());
    }
    args
}

/// The subcommand a bare-session token was probably meant to be (edit
/// distance <= 2), for a `did you mean` hint when the lookup fails.
pub fn misspelt_subcommand(args: &[OsString]) -> Option<&'static str> {
    let token = args.get(first_positional(args)?)?.to_str()?;
    if SUBCOMMANDS.contains(&token) {
        return None;
    }
    SUBCOMMANDS
        .iter()
        .map(|s| (edit_distance(token, s), *s))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, s)| s)
}

/// Damerau-Levenshtein (optimal string alignment) distance over chars: an
/// adjacent transposition (`mxu` -> `mux`) costs 1.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut rows: Vec<Vec<usize>> = vec![(0..=b.len()).collect()];
    for i in 1..=a.len() {
        let mut cur = vec![i];
        for j in 1..=b.len() {
            let prev = &rows[i - 1];
            let mut d = (prev[j - 1] + usize::from(a[i - 1] != b[j - 1]))
                .min(prev[j] + 1)
                .min(cur[j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d = d.min(rows[i - 2][j - 2] + 1);
            }
            cur.push(d);
        }
        rows.push(cur);
    }
    rows[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::FromArgMatches;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(rewrite_bare_session(os(args))).unwrap()
    }

    #[test]
    fn cli_is_valid() {
        Cli::command().debug_assert();
        command().debug_assert();
    }

    #[test]
    fn bare_session_is_menu() {
        let c = parse(&["mnemoloom", "--claude-home", "/x", "abc123"]);
        assert!(
            matches!(c.command, Some(Command::Menu(MenuArgs { session: Some(ref s), .. })) if s == "abc123")
        );
        assert_eq!(c.global.claude_home, Some(PathBuf::from("/x")));
        assert!(parse(&["mnemoloom"]).command.is_none());
        assert!(matches!(
            parse(&["mnemoloom", "info"]).command,
            Some(Command::Info(_))
        ));
    }

    #[test]
    fn global_homes_after_subcommand() {
        let c = parse(&["mnemoloom", "info", "x", "--codex-home", "/c"]);
        assert_eq!(c.global.codex_home, Some(PathBuf::from("/c")));
        let c = parse(&[
            "mnemoloom",
            "--pi-home",
            "/p",
            "abc",
            "--opencode-home",
            "/o",
        ]);
        assert_eq!(c.global.pi_home, Some(PathBuf::from("/p")));
        assert_eq!(c.global.opencode_home, Some(PathBuf::from("/o")));
        assert!(
            matches!(c.command, Some(Command::Menu(MenuArgs { session: Some(ref s), .. })) if s == "abc")
        );
        let Some(Command::Info(i)) = parse(&["mnemoloom", "info", "--agent", "opencode"]).command
        else {
            panic!()
        };
        assert_eq!(i.agent.map(Agent::from), Some(Agent::OpenCode));
    }

    #[test]
    fn trim_flags() {
        let c = parse(&[
            "mnemoloom",
            "trim",
            "s",
            "-a",
            "-20",
            "-t",
            "bash,read",
            "-l",
            "300",
        ]);
        let Some(Command::Trim(t)) = c.command else {
            panic!()
        };
        assert_eq!(
            (t.trim_assistant, t.threshold, t.tools.as_deref()),
            (Some(-20), 300, Some("bash,read"))
        );
        assert!(Cli::try_parse_from(["mnemoloom", "trim-in-place", "s", "-l", "0"]).is_err());
        let c = parse(&["mnemoloom", "trim-in-place", "s", "--json", "-n"]);
        let Some(Command::TrimInPlace(t)) = c.command else {
            panic!()
        };
        assert!(t.json && t.dry_run && t.threshold == 500);
    }

    #[test]
    fn resolve_defaults() {
        let Some(Command::Resolve(r)) = parse(&["mnemoloom", "resolve", "q"]).command else {
            panic!()
        };
        assert_eq!(r.agent, "claude");
        let Some(Command::Search(s)) =
            parse(&["mnemoloom", "search", "--json", "-n", "10", "foo"]).command
        else {
            panic!()
        };
        assert_eq!(
            (s.num_results, s.query.as_deref(), s.agent),
            (Some(10), Some("foo"), SearchAgent::All)
        );
        let Some(Command::Menu(m)) =
            parse(&["mnemoloom", "menu", "x", "--start-screen", "trim_menu"]).command
        else {
            panic!()
        };
        assert_eq!(m.start_screen, StartScreen::TrimMenu);
        assert!(matches!(
            parse(&["mnemoloom", "hook", "resume"]).command,
            Some(Command::Hook(HookCommand::Resume(_)))
        ));
    }

    #[test]
    fn mux_forwards_everything() {
        for args in [
            &["mnemoloom", "mux", "list", "--json"][..],
            &["mnemoloom", "--claude-home", "/x", "mux", "list", "--json"],
        ] {
            let Some(Command::Mux(m)) = parse(args).command else {
                panic!("{args:?}")
            };
            assert_eq!(m.args, os(&["list", "--json"]), "{args:?}");
        }
        let Some(Command::Mux(m)) = parse(&["mnemoloom", "mux", "--help"]).command else {
            panic!()
        };
        assert_eq!(m.args, os(&["--help"]));
        assert!(Cli::command().render_help().to_string().contains("mux"));
    }

    #[test]
    fn hook_and_mux_help_hide_homes() {
        let mut cmd = command();
        cmd.build();
        for path in [&["hook"][..], &["hook", "resume"], &["mux"]] {
            let sub = path
                .iter()
                .fold(&mut cmd, |c, name| c.find_subcommand_mut(name).unwrap());
            let help = sub.render_help().to_string();
            assert!(!help.contains("--claude-home"), "{path:?}: {help}");
        }
        let c = command()
            .try_get_matches_from(["mnemoloom", "hook", "resume", "--codex-home", "/c"])
            .and_then(|m| Cli::from_arg_matches(&m))
            .unwrap();
        assert_eq!(c.global.codex_home, Some(PathBuf::from("/c")));
        let help = command().try_get_matches_from(["mnemoloom", "hook", "--help"]);
        assert_eq!(
            help.unwrap_err().kind(),
            clap::error::ErrorKind::DisplayHelp
        );
        assert!(
            cmd.find_subcommand_mut("info")
                .unwrap()
                .render_help()
                .to_string()
                .contains("--claude-home")
        );
    }

    #[test]
    fn misspelt_subcommands() {
        assert_eq!(
            misspelt_subcommand(&os(&["mnemoloom", "serach", "x"])),
            Some("search")
        );
        assert_eq!(
            misspelt_subcommand(&os(&["mnemoloom", "--codex-home", "/c", "lineag"])),
            Some("lineage")
        );
        assert_eq!(misspelt_subcommand(&os(&["mnemoloom", "mxu"])), Some("mux"));
        assert_eq!(edit_distance("serach", "search"), 1);
        assert_eq!(edit_distance("lineag", "lineage"), 1);
        assert_eq!(edit_distance("", "ab"), 2);
        assert_eq!(misspelt_subcommand(&os(&["mnemoloom", "frobnicate"])), None);
        assert_eq!(
            misspelt_subcommand(&os(&["mnemoloom", "search", "x"])),
            None
        );
        assert_eq!(misspelt_subcommand(&os(&["mnemoloom"])), None);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }
}
