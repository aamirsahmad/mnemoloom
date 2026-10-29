---
name: session-search
description: Searches past coding-agent sessions (Claude Code, Codex, pi, opencode) for earlier work, decisions, or code. Use when the user asks about a previous session and the agent cannot start subagents. If subagents are available, use the mnemoloom:session-searcher agent instead.
---

# Session search

Use the `mnemoloom` CLI to find earlier sessions and to summarize them. If
`command -v mnemoloom` gives no result, tell the user to install it with
`cargo install mnemoloom`.

## Steps

1. Select the scope:
   - This project: `--dir "$(pwd)"`.
   - One other project: `--dir <absolute path>` or `--dir <absolute path>:<branch>`.
   - All projects: `-g`.
2. Search with a few specific terms:

   ```bash
   mnemoloom search --json -n 10 --dir "$(pwd)" "<terms>"
   ```

   Each line is one JSON object. The useful fields are `session_id`, `agent`,
   `project`, `branch`, `cwd`, `created`, `modified`, `lines` (number of user
   messages), `first_msg`, `last_msg`, `file_path`, and `snippet`.
3. Rank the results by `snippet`, `first_msg`, and `last_msg`. Select three
   sessions or fewer.
4. Read each selected session selectively:
   - Use `grep -n`, `head`, and `tail` on `file_path`. Do not read a full
     session file. These files can be very large.
   - If `file_path` is empty or is not a session file, export the session and
     read the text in the same way:
     `mnemoloom export <session_id> -o "$(mktemp -d)/session.txt"`.
5. Report for each session: session ID, date, what was done, key decisions,
   files changed, and the end status. Do not show raw JSON.

## Rules

- Do not run interactive commands: `mnemoloom search` without `--json`,
  `mnemoloom menu`, `mnemoloom resume`, `mnemoloom move`, or `mnemoloom trim`
  without flags.
- If nothing matches, say so and suggest other terms or `-g`.
