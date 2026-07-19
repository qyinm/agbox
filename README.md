# agbox

agbox is a local, Rust-native work-handoff runtime for Claude Code and Codex.
It normalizes agent activity into immutable events and exposes project-scoped
work context through a CLI, terminal UI, and local MCP server.

```sh
agbox init --quiet
agbox daemon start
agbox doctor --output json
agbox work current
agbox mcp --provider codex
```

## Scope

- Supports Claude Code and Codex only.
- Stores local state in `~/.agbox/state.db` and encrypted evidence below
  `~/.agbox/evidence`.
- Automatically replays trusted history for at most 90 days. Undated sources
  are baselined at EOF.
- Uses owner-only local IPC and project-scoped MCP reads.

agbox does not execute commands, launch or assign agents, or use cloud,
screen, audio, general ChatGPT, Grok, Cursor, or third-party agent sources.
Those are later phases.

## Migration

Rust v2 is a clean-slate runtime. It never imports or modifies the legacy
`~/.agbox/agbox.db` database. `agbox init` retires the legacy
`com.agboxhq.watcher` LaunchAgent transactionally while preserving its plist
for rollback. Install the previous published npm package to roll back; Rust
state stays separate.

## Packaging

The npm package is a checksum-verified downloader for a macOS arm64 Rust
binary. No Go runtime is shipped in this repository.
