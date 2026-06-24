<!--
  Banner/demo assets are intentionally omitted until they exist in .github/assets/
  (a broken <img> looks worse than none). Drop in agbox-banner.png + demo.gif later
  and uncomment the blocks marked ASSET-PLACEHOLDER.
-->

<div align="center">

  <!-- ASSET-PLACEHOLDER: <img src=".github/assets/agbox-banner.png" alt="agbox" width="100%" /> -->

  <h1>🧠 agbox</h1>

  <p><strong>Workflow memory for AI coding agents.</strong></p>
  <p>
    Your agents keep making the same mistakes.<br/>
    agbox captures the corrections you repeat — and promotes them into reusable skills your agents actually follow.
  </p>

  <p>
    <a href="https://www.npmjs.com/package/@agboxhq/cli"><img src="https://img.shields.io/npm/v/@agboxhq/cli?style=for-the-badge&logo=npm&color=CB3837" alt="npm" /></a>
    <a href="https://github.com/qyinm/agbox/actions"><img src="https://img.shields.io/github/actions/workflow/status/qyinm/agbox/npm-publish.yml?style=for-the-badge&logo=githubactions&logoColor=white&label=build" alt="build" /></a>
    <img src="https://img.shields.io/github/go-mod/go-version/qyinm/agbox?style=for-the-badge&logo=go&logoColor=white&color=00ADD8" alt="Go" />
    <a href="https://github.com/qyinm/agbox/stargazers"><img src="https://img.shields.io/github/stars/qyinm/agbox?style=for-the-badge&logo=github&color=gold" alt="Stars" /></a>
  </p>

  <p>
    <img src="https://img.shields.io/badge/Claude%20Code-compatible-6366F1?style=for-the-badge&logo=anthropic&logoColor=white" alt="Claude Code" />
    <img src="https://img.shields.io/badge/Codex-compatible-10B981?style=for-the-badge&logo=openai&logoColor=white" alt="Codex" />
    <img src="https://img.shields.io/badge/Cursor-compatible-111111?style=for-the-badge" alt="Cursor" />
    <img src="https://img.shields.io/badge/Cline-compatible-2563EB?style=for-the-badge" alt="Cline" />
  </p>

  <p><em>Correct your agent once. agbox makes sure you never have to again.</em></p>

</div>

---

## ⚡ The 30-second aha

You've typed `use bun, not npm` to your agent all week.

agbox noticed.

```console
$ agbox inbox

  Promotion Inbox · 3 candidates
  ───────────────────────────────────────────────
  ●  use-bun-not-npm        seen 7×    confidence  high
  ●  pr-summary-format      seen 4×    confidence  med
  ○  conventional-commits   seen 3×    confidence  low

  → agbox evidence use-bun-not-npm     see why it's a candidate
  → agbox approve  use-bun-not-npm     promote it to a skill
```

You review the evidence, approve it, and agbox writes the rule into the files your
agents already read — so the correction sticks, across every agent.

```console
$ agbox approve use-bun-not-npm --name package-manager
✓ promoted → approved

$ agbox export package-manager --target claude
✓ wrote CLAUDE.md  (wrapped in an agbox:start/end block, backup saved)
  undo anytime:  agbox export rollback <export-id>
```

That's the whole product: **stop repeating yourself to your AI.**

---

## 🚀 Quick start

```bash
# Install (macOS arm64 today; Homebrew + Linux on the roadmap)
npm install -g @agboxhq/cli
agbox beta
```

`npm install` runs `agbox init --quiet` automatically — it creates `~/.agbox/`, installs the
session watcher, installs managed proposal hooks, and ingests existing agent sessions.
Set `AGBOX_SKIP_WATCHER=1` to skip all setup, or `AGBOX_SKIP_CONNECT=1` to keep the
watcher but skip managed proposal hooks.

See the entire loop in a throwaway store, without touching anything real:

```bash
agbox demo
```

Then just work:

```bash
# 1. Code like you always do. Correct your agent like you always do.
#    agbox watches session files in the background.
#    Managed hooks only propose skills and acknowledge created skill files.

# 2. See setup + candidates in one terminal-safe summary
agbox beta                      # best first beta command

# 3. Review what it learned
agbox review                    # interactive TUI: evidence, approve, export

# 4. Check watcher and managed hook health anytime
agbox status                    # watcher state, last sync, correction counts
agbox doctor                    # full health check
```

---

## 🎁 What you get

### See *why* a correction became a candidate — before anything touches your config

```console
$ agbox evidence use-bun-not-npm

  Candidate · use-bun-not-npm
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
  → agbox approve use-bun-not-npm --name package-manager
```

No black box. Every candidate is a readable **Evidence Card** you can trust or reject.

### Prove it actually worked

```console
$ agbox impact use-bun-not-npm

  Repeat corrections · before vs after promotion
  ───────────────────────────────────────────────
  use-bun-not-npm     7  →  0     ✓ stopped recurring
```

> Output above is illustrative — run `agbox demo` to see the real thing end to end.

---

## 🧩 How it works

agbox keeps a tiny, local store in your home directory — like `.git/`, but for the
workflows your agents keep forgetting.

```
  ingest     →    cluster    →    review      →    export
 ┌────────┐      ┌────────┐      ┌─────────┐      ┌──────────────┐
 │watcher │      │ scan   │      │ review  │      │ CLAUDE.md    │
 │ session│ ───▶ │ group  │ ───▶ │ approve │ ───▶ │ AGENTS.md    │
 │ files  │      │ repeats│      │ /reject │      │ Cursor·Cline │
 └────────┘      └────────┘      └─────────┘      └──────────────┘
  automatic       confidence      you stay         written where
                  scored          in control       agents read
```

```
~/.agbox/
├── agbox.db          # global SQLite store (sessions, corrections, candidates)
├── exports/          # reversible export backups
└── watcher/          # LaunchAgent state

<project>/.agbox/
├── skill-pack.json   # manifest + integrity hashes (per project)
└── config.toml
```

Ingest is automatic and quiet — no notifications. Promotion is **always** a human
decision. Export is **always** reversible.

Clustering is deterministic and review-first: exact normalized hashes plus a small
workflow taxonomy (package-manager preferences, PR-format rules, API/OpenAPI-sync
rules). agbox never silently installs a detected workflow.

---

## ✨ Features

| | |
|---|---|
| 📥 **Automatic ingest** | A background watcher reads Claude Code, Codex, Cursor, and Grok session files. No manual commits, no copy-paste-into-a-fresh-chat. |
| 💬 **In-context proposals** | Managed hooks can ask before creating a skill when agbox finds a repeated correction. Remove them with `agbox disconnect <agent>`. |
| 🧮 **Smart clustering** | Repeated instructions get normalized, grouped, and confidence-scored — directional prefs like `bun-over-npm` included. |
| 👀 **Beta summary + Review TUI** | `agbox beta` gives a terminal-safe summary; `agbox review` drills into evidence, approval, and export. |
| 📤 **Vendor-neutral export** | One skill → `CLAUDE.md`, `AGENTS.md`, Cursor, Cline. Promote once, every agent obeys. |
| ↩️ **Always reversible** | Every export is backed up and wrapped in markers. `agbox export rollback` undoes it cleanly. |
| 🔒 **Local-first & private** | Everything lives in `~/.agbox/`. Redacted excerpts + hashes by default — raw prompts never leave your machine. |
| 📊 **Impact tracking** | `agbox impact` shows repeat-correction counts before vs after. Proof, not vibes. |
| 🧾 **Audit & doctor** | `agbox audit` produces a shareable report; `agbox doctor` checks your setup. |

---

## 🔌 Works with

agbox is **vendor-neutral by design.** It ingests from the agents you already run and
exports to the formats they already read.

| Ingest from | Export to |
|---|---|
| Claude Code · Codex · Cursor | `CLAUDE.md` |
| | `AGENTS.md` *(read by most modern agents, including OpenClaw)* |
| | `.cursor/rules/*.mdc` *(Cursor)* |
| | `.clinerules/*.md` *(Cline)* |

One correction, promoted once, lands everywhere your agents look.

---

## 🔒 Privacy & local-first

agbox touches your prompts and your config files. That trust is the product, so:

- **Sessions, prompts, and core workflow data stay local.** The global store is `~/.agbox/agbox.db`. Your corrections, candidates, exports, and session ingest never leave your machine unless you explicitly share them (e.g. `agbox audit`).
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
- **Reversible by default.** Every export write is backed up; `agbox export rollback <id>` restores it. Managed proposal hooks can be removed with `agbox disconnect <agent>`.
- **Inspectable.** Open source, with a deterministic compiler — read exactly what gets written before it's written.
- **Auditable.** `agbox audit` supports `private`, `shareable`, and `client` profiles.

---

## 🤔 Why agbox exists

AI agents are brilliant and forgetful. You correct the same thing every session:

- *"use bun, not npm"*
- *"tests go in `__tests__`, not next to the file"*
- *"summary → tests → risk, in that order, in every PR"*

Today that knowledge lives in your head and your patience. You either re-type it forever,
or you hand-maintain a sprawling `CLAUDE.md` you forget to update. Both are friction, and
friction is why most people just… don't.

agbox closes the loop. The corrections you already make become the memory your agents
already follow — captured automatically, reviewed by you, written where it counts.

**North star:** fewer repeated corrections per repo-week after a skill is exported. Not
files shipped — corrections eliminated.

---

## 📟 Command reference

```text
agbox init [--quiet]                initialize ~/.agbox/, install watcher + managed hooks, ingest sessions
agbox beta [--limit 5]              setup health + best candidate summary
agbox demo                          run the full loop in a throwaway store
agbox status                        watcher state, last sync, correction counts
agbox sources                       list discovered session source paths
agbox sync --once                   force a session ingestion pass
agbox watch                         internal daemon (used by LaunchAgent)

agbox capture --agent <a> "text"    record a workflow signal manually

agbox scan                          detect repeated normalized signals
agbox inbox [--state pending]       show Promotion Inbox candidates
agbox discover                      scan + evidence + next-step commands
agbox review                        interactive TUI review (primary interface)

agbox evidence <id>                 explain why a candidate exists
agbox approve <id> [--name …]       move a candidate to approved
agbox reject  <id>                  reject a candidate
agbox compile <id> [--target …]     render an approved skill (no write)
agbox export  [id…] [--target …]    dry-run or apply an export plan
agbox export rollback <export-id>   restore the file backup for an export
agbox connect <agent>               install managed proposal hooks
agbox disconnect <agent>            remove managed proposal hooks

agbox impact <id>                   repeat counts before vs after export
agbox audit  [--profile …]          generate a workflow audit pack
agbox manifest verify               verify .agbox/skill-pack.json hashes
agbox doctor                        local health check

agbox telemetry off                 disable anonymous usage stats (default: on)
agbox telemetry on                  re-enable after opt-out
agbox telemetry status              show whether telemetry is on or off
```

Run `agbox <command> --help` for command-specific options.

---

## 🛠️ Development

```bash
git clone https://github.com/qyinm/agbox
cd agbox
go test ./...
go run ./cmd/agbox --help
```

The npm package is published by GitHub Actions from `npm/cli` (the `Publish npm package`
workflow, or pushing a `v*` tag).

---

## 🤝 Contributing

agbox is Go, local-first, and small enough to read in an afternoon. Issues, ideas, and
PRs are all welcome — [open an issue](https://github.com/qyinm/agbox/issues) to start.

## 📄 License

[MIT](LICENSE) © qyinm

---

<div align="center">
  <sub>Built for people who tell their agents the same thing twice. ⌘</sub>
</div>
