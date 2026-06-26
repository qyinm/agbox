<!--
  Banner/demo assets are intentionally omitted until they exist in .github/assets/
  (a broken <img> looks worse than none). Drop in agbox-banner.png + demo.gif later
  and uncomment the blocks marked ASSET-PLACEHOLDER.
-->

<div align="center">

  <!-- ASSET-PLACEHOLDER: <img src=".github/assets/agbox-banner.png" alt="agbox" width="100%" /> -->

  <h1>agbox</h1>

  <p><strong>Workflow memory for AI coding agents.</strong></p>
  <p>
    Your agents keep making the same mistakes.<br/>
    agbox records repeated corrections and recurring workflow prompts, suggests replay plans when they match your next request, and saves durable behavior only after you approve it.
  </p>

  <p>
    <a href="https://www.npmjs.com/package/@agboxhq/cli"><img src="https://img.shields.io/npm/v/@agboxhq/cli?style=for-the-badge&logo=npm&color=CB3837" alt="npm" /></a>
    <a href="https://github.com/qyinm/agbox/actions"><img src="https://img.shields.io/github/actions/workflow/status/qyinm/agbox/npm-publish.yml?style=for-the-badge&logo=githubactions&logoColor=white&label=build" alt="build" /></a>
    <img src="https://img.shields.io/github/go-mod/go-version/qyinm/agbox?style=for-the-badge&logo=go&logoColor=white&color=00ADD8" alt="Go" />
    <a href="https://github.com/qyinm/agbox/stargazers"><img src="https://img.shields.io/github/stars/qyinm/agbox?style=for-the-badge&logo=github&color=gold" alt="Stars" /></a>
  </p>

  <p>
    <img src="https://img.shields.io/badge/Claude%20Code-ingest-6366F1?style=for-the-badge&logo=anthropic&logoColor=white" alt="Claude Code ingest" />
    <img src="https://img.shields.io/badge/Codex-ingest-10B981?style=for-the-badge&logo=openai&logoColor=white" alt="Codex ingest" />
    <img src="https://img.shields.io/badge/Cursor-ingest-111111?style=for-the-badge" alt="Cursor ingest" />
    <img src="https://img.shields.io/badge/Grok-ingest-000000?style=for-the-badge" alt="Grok ingest" />
    <img src="https://img.shields.io/badge/Cline-export-2563EB?style=for-the-badge" alt="Cline export" />
  </p>

  <p><em>Repeat a workflow once too often. agbox makes sure you never have to again.</em></p>

</div>

---

## The 30-second aha

You've typed `use bun, not npm` or `analyze the current project first` to your agent all week.

agbox noticed.

```bash
npm install -g @agboxhq/cli   # macOS Apple Silicon (arm64) only today
agbox inbox                   # Recorded Workflows and replay plans
```

```console
$ agbox inbox

  Recorded Workflows (showing 3 all)

  1. Current Project Analysis (cand_abc123)
     lifecycle=Ready to replay confidence=high repeats=4 projects=1 source=prompt_pattern
     when: When the user asks to analyze the current project, repository, codebase, or progress.
     replay:
       1. Inspect repository structure, language stack, package metadata, and tests for this request.
       2. Summarize what the project does, key entry points, current state, and notable risks.
       3. Ground conclusions in files or commands inspected during this request.
     safety: Replay injects instructions and context for the current request only; it does not re-run prior commands or create a persistent skill.
```

When you later type the same kind of prompt, agbox asks your agent to offer an
apply-once replay plan. If it works, agbox can ask at the end of the session
whether to save that workflow for future automatic use.

```console
$ agbox apply cand_abc123 --agent codex --project agbox
cand_abc123 -> applied once

# Later, after explicit approval, a native SKILL.md containing agbox_candidate_id
# marks the workflow as saved for future use.
```

That's the whole product: **record the workflow automatically, replay it once, then save it only when you say so.**

---

## Quick start

> **Platform:** `@agboxhq/cli` on npm is **macOS Apple Silicon (arm64) only** today.
> Intel Mac, Linux, and Homebrew are on the roadmap. Other platforms: clone and `go install` from source.

```bash
npm install -g @agboxhq/cli
agbox
agbox beta
```

`npm install` runs `agbox init --quiet` automatically. It creates `~/.agbox/`, installs the
session watcher, installs managed workflow hooks, and ingests existing agent sessions.
Set `AGBOX_SKIP_WATCHER=1` to skip all setup, or `AGBOX_SKIP_CONNECT=1` to keep the
watcher but skip managed workflow hooks.

See the entire loop in a throwaway store, without touching anything real:

```bash
agbox demo
```

Run bare `agbox` in an interactive terminal to open the workspace dashboard.
Human-facing commands such as `agbox status`, `agbox inbox`, `agbox sources`,
`agbox doctor`, `agbox repair`, `agbox evidence <id>`, and `agbox help` open the
matching workspace screen. Pipes, redirected output, and scripts keep the
line-oriented output automatically; use `--plain` when you want that output from
an interactive shell.

Then just work:

```bash
# 1. Code like you always do. Correct your agent or repeat workflow prompts like you always do.
#    agbox watches session files in the background.
#    Managed hooks suggest replay, ask whether to save for future, and acknowledge created skill files.
#    If a hook misses a SKILL.md write, agbox reconciles files with agbox_candidate_id
#    when you run status, beta, doctor, or sync.

# 2. Open the workspace dashboard
agbox                           # overview, status bar, workflows, sources, repair, help

# 3. See setup + a curated workflow summary
agbox beta                      # best first beta command
agbox beta --sync               # force a refresh when you want one

# 4. Review what it learned
agbox inbox                     # workspace screen for Recorded Workflow cards and replay plans
agbox inbox --plain             # line-oriented output for scripts or copy/paste
agbox review                    # interactive TUI: evidence, replay plan, approve, export

# 5. Check watcher and managed hook health anytime
agbox status                    # workspace screen for watcher state, sync, and counts
agbox status --plain            # line-oriented watcher state, sync, and counts
agbox doctor                    # workspace screen for full health check
```

---

## What you get

### See *why* a workflow was recorded before anything becomes persistent

```console
$ agbox evidence use-bun-not-npm

  Recorded Workflow · use-bun-not-npm
  ─────────────────────────────────────────────
  Seen        7 times · 2 sessions
  Agents      claude_code, codex_cli
  Confidence  high  (repeated · multi-agent)

  Signals
    "use bun, not npm"
    "stop using npm — we're a bun project"
    "bun install, never npm i"

  Suggested rule
    Always use bun as the package manager. Never use npm.
  ─────────────────────────────────────────────
  > agbox inbox
```

No black box. Every Recorded Workflow is backed by readable evidence you can trust, snooze, reject, apply once, or save for future.

### Prove it actually worked

```console
$ agbox impact use-bun-not-npm

  Repeat corrections · before vs after saved workflow
  ───────────────────────────────────────────────
  use-bun-not-npm     7  ->  0    stopped recurring
```

> Output above is illustrative — run `agbox demo` to see the real thing end to end.

---

## How it works

agbox keeps a tiny, local store in your home directory — like `.git/`, but for the
workflows your agents keep forgetting.

```
  ingest     ->   cluster    ->   replay      ->   save
 ┌────────┐      ┌────────┐      ┌─────────┐      ┌──────────────┐
 │watcher │      │ scan   │      │ apply   │      │ SKILL.md     │
 │ session│ ───▶ │ group  │ ───▶ │ once    │ ───▶ │ after        │
 │ files  │      │ repeats│      │ /reject │      │ approval     │
 └────────┘      └────────┘      └─────────┘      └──────────────┘
  automatic       confidence      current request  future reuse
                  scored          only             requires yes
```

```
~/.agbox/
├── agbox.db          # global SQLite store (sessions, corrections, prompt events, workflows)
├── exports/          # reversible export backups
└── watcher/          # LaunchAgent state

<project>/.agbox/
├── skill-pack.json   # manifest + integrity hashes (per project)
└── config.toml
```

Ingest is automatic and quiet. Replay is instruction-only and scoped to the current
request. Saving a workflow for future use is **always** a separate human decision.
Export remains reversible.

Clustering is deterministic and review-first: exact normalized hashes plus a small
workflow taxonomy (package-manager preferences, PR-format rules, API/OpenAPI-sync
rules). agbox never silently installs a detected workflow.

---

## Features

| | |
|---|---|
| **Automatic ingest** | A background watcher reads Claude Code, Codex, Cursor, and Grok session files. No manual commits, no copy-paste-into-a-fresh-chat. |
| **In-context replay** | Managed hooks can suggest an apply-once replay plan when the current prompt matches a Recorded Workflow. At session stop, agbox can separately ask whether to save that workflow for future use. |
| **Smart clustering** | Repeated instructions get normalized, grouped, and confidence-scored — directional prefs like `bun-over-npm` included. |
| **Workspace + Review TUI** | Bare `agbox` opens a workspace with status, sources, workflows, repair, and help. `agbox inbox` opens Recorded Workflow cards with replay plans; `agbox review` drills into evidence, approval, and export. |
| **Vendor-neutral export** | A saved workflow can be exported to `CLAUDE.md`, `AGENTS.md`, Cursor, and Cline formats when you want durable agent behavior. |
| **Always reversible** | Every export is backed up and wrapped in markers. `agbox export rollback` undoes it cleanly. |
| **Local-first & private** | Sessions and workflow data stay in `~/.agbox/`. Redacted excerpts + hashes by default — raw prompts stay local. Anonymous usage counters only (opt-out: `agbox telemetry off`). |
| **Impact tracking** | `agbox impact` shows repeat-correction counts before vs after. Proof, not vibes. |
| **Audit & doctor** | `agbox audit` produces a shareable report; `agbox doctor` checks your setup. |

---

## Works with

agbox is **vendor-neutral by design.** It ingests from the agents you already run and
exports to the formats they already read.

| Ingest from | Export to |
|---|---|
| Claude Code · Codex · Cursor · Grok | `CLAUDE.md` |
| Managed workflow hooks: Claude · Codex · Grok | `AGENTS.md` *(read by most modern agents, including OpenClaw)* |
| | `.cursor/rules/*.mdc` *(Cursor)* |
| | `.clinerules/*.md` *(Cline — export only)* |

One recorded workflow can be replayed once for the current request, then saved for future agent use after explicit approval.

---

## Privacy & Local-First

agbox touches your prompts and your config files. That trust is the product, so:

- **Sessions, prompts, and core workflow data stay local.** The global store is `~/.agbox/agbox.db`. Your corrections, prompt events, Recorded Workflows, replay applications, exports, and session ingest never leave your machine unless you explicitly share them (e.g. `agbox audit`).
- **Anonymous usage stats (on by default).** Telemetry is enabled unless you opt out with `agbox telemetry off` or `AGBOX_TELEMETRY=0`. agbox sends only:
  - `agbox_install_completed` once (install/version signal)
  - `agbox_daily_active` at most once per UTC day (includes `streak_days`)
  
  Events go to PostHog (maintainer analytics). Your distinct ID is a random UUID — not your hostname, username, or machine fingerprint. Payloads include `app` (`agbox`), `agbox_version`, `os_family`, and `arch` only.

  **Turn off telemetry:**

  ```bash
  agbox telemetry off
  ```

  Or set `AGBOX_TELEMETRY=0` in your shell. Check status with `agbox telemetry status` or `agbox doctor`.
- **Redacted by default.** Persisted signals are short redacted excerpts + a hash + metadata. Raw text is opt-in via `--raw`; session ingest never stores full transcripts.
- **Reversible by default.** Every export write is backed up; `agbox export rollback <id>` restores it. Managed workflow hooks can be removed with `agbox disconnect <agent>`.
- **Inspectable.** Open source, with a deterministic compiler — read exactly what gets written before it's written.
- **Auditable.** `agbox audit` supports `private`, `shareable`, and `client` profiles.

---

## Why agbox Exists

AI agents are brilliant and forgetful. You correct the same thing every session:

- *"use bun, not npm"*
- *"tests go in `__tests__`, not next to the file"*
- *"summary -> tests -> risk, in that order, in every PR"*
- *"analyze the current project before recommending changes"*

Today that knowledge lives in your head and your patience. You either re-type it forever,
or you hand-maintain a sprawling `CLAUDE.md` you forget to update. Both are friction, and
friction is why most people just… don't.

agbox closes the loop. The corrections you make and workflow prompts you repeat become
the memory your agents already follow — captured locally, reviewed by you, written where it counts.

**North star:** fewer repeated corrections and recurring workflow prompts per repo-week after a skill is exported. Not
files shipped — repetition eliminated.

---

## Command Reference

```text
agbox                               open the interactive workspace dashboard
agbox init [--quiet]                initialize ~/.agbox/, install watcher + managed hooks, ingest sessions
agbox beta [--limit 5] [--sync]     setup health + curated workflow summary
agbox demo                          run the full loop in a throwaway store
agbox status [--plain]              workspace status screen; --plain for line output
agbox sources [--plain]             workspace sources screen; --plain for line output
agbox sync --once                   force a standalone session ingestion pass
agbox watch                         internal daemon (used by LaunchAgent)

agbox capture --agent <a> "text"    record a workflow signal manually

agbox scan                          detect repeated normalized signals
agbox inbox [--plain] [--state …|all]
                                    workspace workflow cards; --plain for line output
agbox discover                      scan + evidence + next-step commands
agbox review                        interactive TUI review (primary interface)

agbox evidence [--plain] <id>       workspace evidence detail; --plain for line output
agbox apply <id> [--agent …]        record that replay was applied once
agbox approve <id> [--name …]       approve a Recorded Workflow for export
agbox reject  <id>                  reject a Recorded Workflow
agbox snooze  <id>                  snooze a Recorded Workflow (24h)
agbox accept  <id> [--skill-path …] mark saved for future after SKILL.md creation
agbox compile <id> [--target …]     render an approved skill (no write)
agbox export  <id>… [--target …]    dry-run or apply an export plan
agbox export rollback <export-id>   restore the file backup for an export
agbox connect <agent>               install managed workflow hooks (claude|codex|grok)
agbox disconnect <agent>            remove managed workflow hooks
agbox hook propose|replay|save|acknowledge …
                                    hook entrypoints (used by agent hook configs)

agbox impact <id>                   repeat counts before vs after export
agbox audit  [--profile …]          generate a workflow audit pack
agbox manifest verify               verify .agbox/skill-pack.json hashes
agbox doctor [--plain]              workspace health check; --plain for line output
agbox repair [--plain]              workspace repair screen; --plain for line output
agbox debug-bundle [--out …]        write a local debug bundle for troubleshooting

agbox telemetry off                 disable anonymous usage stats (default: on)
agbox telemetry on                  re-enable after opt-out
agbox telemetry status              show whether telemetry is on or off
```

Run `agbox <command> --help` for command-specific options.

---

## Development

```bash
git clone https://github.com/qyinm/agbox
cd agbox
go test ./...
go run ./cmd/agbox --help
```

The npm package is published by GitHub Actions from `npm/cli` (the `Publish npm package`
workflow, or pushing a `v*` tag).

---

## Contributing

agbox is Go, local-first, and small enough to read in an afternoon. Issues, ideas, and
PRs are all welcome — [open an issue](https://github.com/qyinm/agbox/issues) to start.

## License

[MIT](LICENSE) © qyinm

---

<div align="center">
  <sub>Built for people who tell their agents the same thing twice.</sub>
</div>
