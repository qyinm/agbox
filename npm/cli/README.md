# @agboxhq/cli

`@agboxhq/cli` is the thin macOS arm64 installer for the Rust agbox runtime.
It downloads one checksum-pinned native binary, verifies it before execution,
then runs `agbox init --quiet`.

Supported providers are Claude Code and Codex. agbox is local-only: it records
work context for handoff, but does not start agents, execute work, or assign
tasks.

```sh
npm install -g @agboxhq/cli
agbox doctor --output json
agbox work current
```

The runtime stores fresh state under `~/.agbox/state.db`. It does not import,
open, or delete the legacy `~/.agbox/agbox.db` database. Roll back by
installing the previous published package version; that version remains
independent of Rust state.

The first release supports macOS Apple Silicon only. The installer rejects
unsupported platforms and never falls back to a Go implementation.
