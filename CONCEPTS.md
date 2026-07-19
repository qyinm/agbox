# agbox concepts

## Activity events and evidence

Claude Code and Codex source records become immutable normalized activity
events. Native transcript material remains encrypted local evidence; it is not
treated as authority for arbitrary agent instructions.

## Work handoff

The deterministic work graph creates project-scoped work contracts containing
status, completed steps, blockers, artifacts, verification, and bounded next
actions. Claude and Codex can read the same contract over local MCP.

## Boundaries

agbox is not an agent executor or work-assignment system. It provides local
handoff context only. Screen capture, audio capture, cloud sync, and providers
beyond Claude Code and Codex are out of scope for this release.

## State and privacy

The Rust runtime owns `~/.agbox/state.db`; it does not read or migrate the
legacy `~/.agbox/agbox.db`. Evidence is encrypted and owner-only. Explicit
forget actions remove work or project data without touching legacy state.
