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
agbox doctor
```

See the entire loop in a throwaway store, without touching anything real:

```bash
agbox demo
```

Then wire it into your agents and just work:

```bash
# 1. Connect agbox — installs a reversible capture hook (JSON only, preserves your other hooks)
agbox connect all --dry-run     # preview the exact change
agbox connect all --apply       # apply it

# 2. Code like you always do. Correct your agent like you always do.
#    agbox listens in the background — no manual commits, no copy-paste.

# 3. See what it learned
agbox discover                  # scan + show evidence + next steps
```

Disconnect just as cleanly, anytime: `agbox disconnect all --apply`.

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

agbox keeps a tiny, local store next to your code — like `.git/`, but for the
workflows your agents keep forgetting.

```
  capture    →    cluster    →    review      →    export
 ┌────────┐      ┌────────┐      ┌─────────┐      ┌──────────────┐
 │ hook   │      │ scan   │      │ inbox   │      │ CLAUDE.md    │
 │ every  │ ───▶ │ group  │ ───▶ │ approve │ ───▶ │ AGENTS.md    │
 │ turn   │      │ repeats│      │ /reject │      │ Cursor·Cline │
 └────────┘      └────────┘      └─────────┘      └──────────────┘
  automatic       confidence      you stay         written where
                  scored          in control       agents read
```

```
.agbox/
├── objects/          # content-addressed signal blobs
├── index.db          # SQLite query index
├── skill-pack.json   # manifest + integrity hashes
└── config.toml
```

Capture is automatic. Promotion is **always** a human decision. Export is **always**
reversible.

Clustering is deterministic and review-first: exact normalized hashes plus a small
workflow taxonomy (package-manager preferences, PR-format rules, API/OpenAPI-sync
rules). agbox never silently installs a detected workflow.

---

## ✨ Features

| | |
|---|---|
| 📥 **Automatic capture** | A reversible hook into Claude Code & Codex records every correction. No manual commits, no copy-paste-into-a-fresh-chat. |
| 🧮 **Smart clustering** | Repeated instructions get normalized, grouped, and confidence-scored — directional prefs like `bun-over-npm` included. |
| 👀 **Promotion Inbox** | Nothing touches your config without you. Review each candidate as a readable Evidence Card. |
| 📤 **Vendor-neutral export** | One skill → `CLAUDE.md`, `AGENTS.md`, Cursor, Cline. Promote once, every agent obeys. |
| ↩️ **Always reversible** | Every export is backed up and wrapped in markers. `agbox export rollback` undoes it cleanly. |
| 🔒 **Local-first & private** | Everything lives in `.agbox/`. Redacted excerpts + hashes by default — raw prompts never leave your machine. |
| 📊 **Impact tracking** | `agbox impact` shows repeat-correction counts before vs after. Proof, not vibes. |
| 🧾 **Audit & doctor** | `agbox audit` produces a shareable report; `agbox doctor` checks your setup. |

---

## 🔌 Works with

agbox is **vendor-neutral by design.** It captures from the agents you already run and
exports to the formats they already read.

| Capture from | Export to |
|---|---|
| Claude Code · Codex | `CLAUDE.md` |
| | `AGENTS.md` *(read by most modern agents, including OpenClaw)* |
| | `.cursor/rules/*.mdc` *(Cursor)* |
| | `.clinerules/*.md` *(Cline)* |

One correction, promoted once, lands everywhere your agents look.

---

## 🔒 Privacy & local-first

agbox touches your prompts and your config files. That trust is the product, so:

- **Everything stays local.** The store is `.agbox/` in your project. Nothing is uploaded.
- **Redacted by default.** Persisted signals are short redacted excerpts + a hash + metadata. Raw text is opt-in via `--raw`; hook capture never stores raw prompts or full transcripts.
- **Reversible by default.** Every config write is backed up; `agbox export rollback <id>` restores it. `connect` is non-mutating unless you pass `--apply`, and only writes agbox-managed JSON hook entries.
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
agbox init                          initialize the local .agbox/ store
agbox demo                          run the full loop in a throwaway store

agbox connect <claude|codex|all>    install a reversible capture hook  [--dry-run|--apply]
agbox disconnect <…>                remove only agbox-managed hook entries
agbox hook <claude|codex>           capture from an agent hook payload
agbox capture --agent <a> "text"    record a workflow signal manually

agbox scan                          detect repeated normalized signals
agbox inbox [--state pending]       show Promotion Inbox candidates
agbox discover                      scan + evidence + next-step commands
agbox review                        interactive TUI review

agbox evidence <id>                 explain why a candidate exists
agbox approve <id> [--name …]       move a candidate to approved
agbox reject  <id>                  reject a candidate
agbox compile <id> [--target …]     render an approved skill (no write)
agbox export  [id…] [--target …]    dry-run or apply an export plan
agbox export rollback <export-id>   restore the file backup for an export

agbox impact <id>                   repeat counts before vs after export
agbox audit  [--profile …]          generate a workflow audit pack
agbox manifest verify               verify .agbox/skill-pack.json hashes
agbox doctor                        local health check
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
