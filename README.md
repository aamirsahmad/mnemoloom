# mnemoloom

Find, resume, trim and hand off your coding-agent sessions.
Supports Claude Code, Codex, pi and opencode.
See all of your running agents in tmux on one screen.

mnemoloom is one fast binary for macOS and Linux. It has no runtime dependencies.

![CI](https://github.com/aamirsahmad/mnemoloom/actions/workflows/ci.yml/badge.svg)
[![crates.io](https://img.shields.io/crates/v/mnemoloom.svg)](https://crates.io/crates/mnemoloom)

## Features

- **Search.** Full-text search over all Claude Code, Codex, pi and opencode sessions. Use the terminal UI, or get JSON lines for scripts and agents.
- **Resume.** Open a session again in its agent, in the correct directory and account.
- **Trim.** Make a session smaller. mnemoloom truncates large tool results and keeps a backup.
- **Roll over.** Start a fresh session that continues the work of an old session. The new session links back to its parent.
- **Port.** Convert a session from Claude Code to Codex, or from Codex to Claude Code.
- **Lineage.** Follow a session back to its original, or forward to the sessions made from it.
- **Session operations.** Show, copy, move, export, query or delete a session.
- **mux.** See every coding agent that runs in a tmux pane, with a live preview. Jump to an agent with one key.
- **Claude Code plugin.** Type `>resume` or `>trim` in a Claude Code prompt. The plugin also adds a session-search agent and skills.

## Requirements

- macOS or Linux.
- One or more of Claude Code, Codex, pi and opencode, to resume sessions.
- tmux, for `mnemoloom mux`.
- Rust 1.90 or later, to build from source.

## Install

Install from crates.io:

```bash
cargo install mnemoloom
```

Or install a binary from [GitHub Releases](https://github.com/aamirsahmad/mnemoloom/releases):

```bash
v=v0.1.0
t=aarch64-apple-darwin   # or x86_64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu
curl -LO "https://github.com/aamirsahmad/mnemoloom/releases/download/$v/mnemoloom-$v-$t.tar.gz"
tar -xzf "mnemoloom-$v-$t.tar.gz"
install "mnemoloom-$v-$t/mnemoloom" ~/.local/bin/
```

Make sure that `~/.local/bin` is on your `PATH`.
To check the download, compare it with `SHA256SUMS` on the release page.
If macOS blocks the binary, run `xattr -d com.apple.quarantine ~/.local/bin/mnemoloom`.

Optional: add the short command `mloom`. It is a link to the same binary:

```bash
ln -s "$(command -v mnemoloom)" "$(dirname "$(command -v mnemoloom)")/mloom"
```

## Agent support

| Feature | Claude Code | Codex | pi | opencode |
|---|---|---|---|---|
| Search, info, lineage, export, query | Yes | Yes | Yes | Yes |
| Resume | Yes | Yes | Yes | Yes |
| mux detection | Yes | Yes | Yes | Yes |
| Trim, roll over, port, clone | Yes | Yes | No | No |
| Copy, move, delete | Yes | Yes | No | No |
| Prompt hooks (plugin) | Yes | No | No | No |

mnemoloom reads pi and opencode data. It does not change pi or opencode session data.

## Quick start

```bash
mnemoloom search                # search all sessions for this directory
mnemoloom search -g "auth bug"  # search all directories
mnemoloom resume                # resume the latest session for this directory
mnemoloom 15a6                  # open the action menu for a session ID fragment
mnemoloom mux                   # pick a running agent in tmux
```

The first search builds the index. Later searches update only the changed files.

## Commands

| Command | Purpose |
|---|---|
| `search` | Search sessions. Opens the terminal UI. Use `--json` for JSON lines. |
| `resume`, `clone` | Resume a session, or resume a copy of it. |
| `menu`, `<session>` | Open the action menu for one session. |
| `trim`, `trim-in-place`, `smart-trim` | Make a session smaller. `smart-trim` uses an agent to select content (experimental). |
| `rollover` | Hand off the work to a fresh session with lineage. |
| `port` | Convert a session to the other agent. |
| `lineage`, `find-original`, `find-derived` | Show how sessions relate. |
| `info`, `resolve` | Show session details. Find a session from a name, ID or fragment. |
| `copy`, `move`, `move-account`, `delete` | Manage session files safely. |
| `export`, `export-claude`, `export-codex` | Write a session as text. |
| `query` | Ask an agent a question about a session. |
| `build-index`, `index-stats`, `clear-index` | Manage the search index. |
| `mux` | Show and jump to agents in tmux. See [mux](#mux). |

Run `mnemoloom <command> --help` for all options.

## Search

`mnemoloom search` opens three panes: **Filters**, **Results** and **Preview**.

| Key | Action |
|---|---|
| `/` | Edit the query. |
| `Tab`, `h`/`l` | Go to the next or previous pane. |
| `j`/`k` | Move in the current pane. |
| `Space` | In Filters: turn a filter on or off. |
| `o` | In Filters, on a project: show only this project. |
| `P` | Open the list of all projects. |
| `n`/`N` | In Preview: go to the next or previous match. |
| `Enter` | Resume the session. |
| `a` | Open the actions for the session. |
| `y`, `x`, `t` | Copy the session ID. Export the session. Trim the session. |
| `?` | Show all keys. |
| `Esc` | Clear the query. Press again to quit. |
| `q` | Quit. |

For scripts and agents, use JSON output:

```bash
mnemoloom search --json -g -n 10 "rate limit"
```

Each line is one session. The `lines` field is the number of user messages.

## mux

`mnemoloom mux` lists every Claude Code, Codex, pi and opencode agent in your tmux panes.
It shows the state of each agent: `busy`, `bg`, `input` or `idle`.

| Key | Action |
|---|---|
| Type | Filter the agents. |
| `Enter` | Go to the agent pane. |
| `Tab`, `/` | Go to the list. Go back to the filter. |
| `p` | In the list: show or hide the preview. |
| `r` | In the list: refresh. |
| `?` | In the list: show all keys. |

Use `mnemoloom mux list` for a table, and `mnemoloom mux list --json` for JSON.

## Claude Code plugin

Add the marketplace and install the plugin in Claude Code:

```text
/plugin marketplace add aamirsahmad/mnemoloom
/plugin install mnemoloom@mnemoloom
```

The plugin needs `mnemoloom` on the `PATH` that Claude Code uses.
If you start Claude Code from a GUI, make sure that this `PATH` includes `~/.cargo/bin`.

Type these words as a prompt:

| Prompt | Result |
|---|---|
| `>resume`, `>continue`, `>handoff` | Copy the session ID to the clipboard. Then run `mnemoloom resume <ID>` in a new terminal. |
| `>session` | Copy the session ID to the clipboard. |
| `>trim` | Show what a trim removes. Type `>trim yes` to apply it. |

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `CLAUDE_CONFIG_DIR` | `~/.claude` | Claude Code home. `--claude-home` overrides it. |
| `CODEX_HOME` | `~/.codex` | Codex home. `--codex-home` overrides it. |
| `PI_CODING_AGENT_DIR` | `~/.pi/agent` | pi home. `--pi-home` overrides it. |
| `XDG_DATA_HOME` | `~/.local/share` | opencode data is in `<this>/opencode`. `--opencode-home` overrides it. |
| `MNEMOLOOM_CACHE_DIR` | `$XDG_CACHE_HOME/mnemoloom` or `~/.cache/mnemoloom` | Root for all caches. |
| `MNEMOLOOM_INDEX_DIR` | `<cache>/search-index` | Search index directory. |
| `MNEMOLOOM_MUX_CACHE` | `<cache>/mux.json` | mux scan cache. |
| `MNEMOLOOM_CLAUDE_BIN`, `MNEMOLOOM_CODEX_BIN`, `MNEMOLOOM_PI_BIN`, `MNEMOLOOM_OPENCODE_BIN` | `claude`, `codex`, `pi`, `opencode` | Agent binaries to run. |
| `MNEMOLOOM_TRIM_TIMEOUT` | `60` | Time limit in seconds for `>trim` in the hook. |

mnemoloom reads session files. Only these commands change them:

- `trim-in-place` and `>trim yes`. They make a `.bak` backup first.
- `move` and `move-account`.
- `delete`. It asks you to confirm first.

`trim`, `rollover`, `clone` and `port` write new session files. They do not change the original.
`export` writes to `<project>/exported-sessions/` unless you give `-o`.
mnemoloom never changes pi or opencode data.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
