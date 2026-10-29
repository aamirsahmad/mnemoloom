# mnemoloom plugin for Claude Code

This plugin connects Claude Code to the `mnemoloom` CLI (short alias `mloom`).
With it, you can find, resume, and trim sessions from the Claude Code prompt.

## Contents

### Prompt hook

The hook reads each prompt. When a prompt starts with one of these triggers,
the hook handles it and Claude does not get the prompt.

| Prompt | Result |
|--------|--------|
| `>resume`, `>continue`, `>handoff` | Copies the session ID to the clipboard and shows how to resume the session. |
| `>session` | Copies the session ID to the clipboard. |
| `>trim` | Shows a preview of a trim of this session. |
| `>trim yes` | Applies the trim that you previewed. |
| `>trim cancel` | Cancels the trim. |

### Agent

`mnemoloom:session-searcher` searches past sessions and returns a short
summary: what was done, key decisions, files, session IDs, and dates. The raw
transcripts stay out of your main context.

### Skills

- `session-search`: the same search as the agent, for agents that cannot start
  subagents.
- `recover-context`: after a rollover or a trim, gets the last task from the
  most recent parent session.

### Command

`/recover-context` starts the `recover-context` skill.

## Install

1. Install the CLI: `cargo install mnemoloom`.
2. Make sure that `mnemoloom` is on the PATH that Claude Code uses.
3. In Claude Code, run:

   ```
   /plugin marketplace add aamirsahmad/mnemoloom
   /plugin install mnemoloom@mnemoloom
   ```

## Use

- To find earlier work, ask a question, for example: "Find the session where
  we changed the retry logic." Claude uses the session-searcher agent.
- To continue in a new session, type `>resume`. Paste the command that the
  hook shows into a terminal.
- To make this session smaller, type `>trim`. Read the preview, then type
  `>trim yes` or `>trim cancel`.
- After `mnemoloom rollover`, run `/recover-context` in the new session.
