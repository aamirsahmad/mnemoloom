# Changelog

All notable changes to this project are in this file.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-10-29

### Added

- `mux`: the filter also finds text in the agent conversations, with the `search` index. The index updates in the background when the picker opens. A matching line from the conversation shows below the agent.
- `mux` filter syntax: `"phrase"`, `!word` to exclude and `~word` for a fuzzy match.

### Changed

- `mux`: filter words match as exact text (smart case) in the full agent fields: name, directory, repository, branch, model, session ID and status line. Before, words matched as fuzzy letter sequences in the visible row only. Use `~word` for the old match.

## [0.1.0] - 2026-10-29

### Added

- First release.
- Support for Claude Code, Codex, pi and opencode sessions. pi and opencode support is read-only plus resume.
- Full-text search over Claude Code, Codex, pi and opencode sessions, with a terminal UI and JSON output.
- Search UI with filter, results and preview panes, a project picker and match navigation.
- Resume, clone, trim, trim in place, smart trim and roll over of sessions, and conversion between Claude Code and Codex.
- Session lineage: `lineage`, `find-original`, `find-derived`.
- Session operations: `info`, `resolve`, `copy`, `move`, `move-account`, `delete`, `export`, `query`.
- `mux`: list and jump to coding agents in tmux panes, with a live preview.
- Claude Code plugin with the `>resume`, `>session` and `>trim` prompt hooks, a session-search agent and skills.

[Unreleased]: https://github.com/aamirsahmad/mnemoloom/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/aamirsahmad/mnemoloom/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aamirsahmad/mnemoloom/releases/tag/v0.1.0
