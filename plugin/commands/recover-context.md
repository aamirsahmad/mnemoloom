---
description: Recover the context of the last task from the most recent parent session (after a mnemoloom rollover or trim)
---

Use the `recover-context` skill. Find the most recent parent session in the
session lineage. Get a short summary of the last task: its state, open issues,
key decisions, and the files that changed. If you can start subagents, give
the reading to the `mnemoloom:session-searcher` agent. Do not read full session
files into your own context.

$ARGUMENTS
