---
name: recover-context
description: Recovers the context of the last task from the most recent parent session after a mnemoloom rollover or trim. Use when the first message of this session shows a session lineage, or when the user asks to continue the work of the previous session.
---

# Recover context

After `mnemoloom rollover` or a trim, the first message of the new session
lists the parent sessions (the lineage). The most recent parent has the work
that this session must continue.

## Steps

1. Find the most recent parent session in the lineage. Note its session ID
   and its file path, if the lineage shows one. If the lineage is not in the
   first message, run `mnemoloom lineage --json` and use the nearest parent.
2. Get a summary of the last task in that session:
   - If you can start subagents, give the work to the
     `mnemoloom:session-searcher` agent. Give it the session ID and the file
     path. Ask for the items in step 3.
   - If you cannot start subagents, use the `session-search` skill. Read the
     end of the session first, because the last task is there.
3. Extract these items:
   - The last task that the user asked for.
   - The state of that task: done, partly done, or not started.
   - Open issues, errors, and next steps.
   - Key decisions and the reasons for them.
   - Files that were created or changed.
4. Give the user a short summary of these items. Then ask what to do next, or
   continue the task if the user already said to continue.

## Rules

- Do not read a full session file or a full export into your own context. Use
  `grep`, `head`, and `tail`, or let the subagent do the reading.
- Keep the summary short. Show only what is necessary to continue the work.
