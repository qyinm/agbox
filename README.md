# agbox

Hidden workflows -> reusable agent skills.

agbox is a local-first CLI that finds repeated AI-agent workflow signals and turns
them into reviewable, exportable skills for `AGENTS.md`, `CLAUDE.md`, Cursor,
Cline, and Codex-style project instructions.

## v0.1 loop

```sh
agbox init
agbox capture --agent codex "Use bun, not npm."
agbox capture --agent claude "Use bun, not npm."
agbox scan
agbox inbox
agbox evidence <candidate-id>
agbox approve <candidate-id> --name package-manager-workflow
agbox export <candidate-id> --target agents-md --dry-run
agbox export <candidate-id> --target agents-md
agbox impact <candidate-id>
agbox audit --profile private --out agbox-audit.md
```

Rollback stays one command away:

```sh
agbox export rollback <export-id>
```

## Privacy defaults

- Capture stores normalized hashes, metadata, and redacted excerpts.
- Raw text storage is opt-in with `--raw`.
- Hook capture uses hash+metadata by default.
- Audit exports support `private`, `shareable`, and `client` profiles.

## Commands

- `agbox capture`: explicitly record a workflow signal.
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
