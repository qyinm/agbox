# agbox

Hidden workflows -> reusable agent skills.

agbox is a local-first CLI that finds repeated AI-agent workflow signals and turns
them into reviewable, exportable skills for `AGENTS.md`, `CLAUDE.md`, Cursor,
Cline, and Codex-style project instructions.

## North star

Reduce repeated corrections per repo-week after a skill is exported.

agbox is not trying to count exported files. The product loop is:

1. Capture workflow corrections from local agent sessions.
2. Cluster exact repeats and close semantic repeats into reviewable candidates.
3. Let the user approve, edit, export, and roll back a portable skill.
4. Measure whether the same correction stops recurring after export.

## Install from source

```sh
go install ./cmd/agbox
```

## Hook capture

See the loop immediately without touching your real store:

```sh
agbox demo
```

Then connect agbox to your AI terminals once and keep working normally:

```sh
agbox connect all --dry-run
agbox connect all --apply
```

When you want to see what repeated workflow instructions agbox found:

```sh
agbox discover
```

`connect` is non-mutating unless `--apply` is passed. It writes only JSON hook
config for Codex (`~/.codex/hooks.json`) and Claude Code
(`~/.claude/settings.json`), preserving unrelated hooks. To remove agbox without
touching other hooks:

```sh
agbox disconnect all --apply
```

## v0.1 loop

```sh
agbox init
agbox capture --agent codex "Use bun, not npm."
agbox capture --agent claude "Use bun, not npm."
agbox scan
agbox inbox
agbox discover
agbox evidence <candidate-id>
agbox approve <candidate-id> --name package-manager-workflow
agbox export <candidate-id> --target agents-md --dry-run
agbox export <candidate-id> --target agents-md
agbox impact <candidate-id>
agbox audit --profile private --out agbox-audit.md
```

Discovery starts with deterministic clustering: exact normalized hashes, plus a
small workflow taxonomy for common correction patterns such as package-manager
preferences, PR format rules, and API route/OpenAPI sync rules. This is intentionally
local and review-first; agbox never silently installs a detected workflow.

Rollback stays one command away:

```sh
agbox export rollback <export-id>
```

## Privacy defaults

- Capture stores normalized hashes, metadata, and redacted excerpts.
- Raw text storage is opt-in with `--raw`.
- Hook capture stores a short redacted excerpt, hash, and metadata by default.
- Hook capture does not store raw prompts or full transcripts.
- Audit exports support `private`, `shareable`, and `client` profiles.

## Commands

- `agbox capture`: explicitly record a workflow signal.
- `agbox hook`: capture a workflow signal from an agent hook payload.
- `agbox connect`: install a reversible Claude/Codex prompt hook.
- `agbox disconnect`: remove only agbox-managed Claude/Codex hook entries.
- `agbox discover`: scan, show evidence, and print next promotion commands.
- `agbox demo`: show the full loop in a temporary store without touching user data.
- `agbox scan`: detect repeated normalized signals.
- `agbox inbox`: show Promotion Inbox candidates.
- `agbox evidence`: explain why a candidate exists.
- `agbox approve` / `agbox reject`: move candidates through review states.
- `agbox compile`: render an approved candidate without writing files.
- `agbox export`: dry-run or apply export plans.
- `agbox export rollback`: restore the file backup for an export.
- `agbox manifest verify`: verify `.agbox/skill-pack.json` hashes.
- `agbox impact`: compare repeat counts before and after export.
- `agbox audit`: generate a workflow audit pack.
- `agbox doctor` / `agbox debug-bundle`: local health and sanitized support data.

## Development

```sh
go test ./...
go run ./cmd/agbox --help
```
