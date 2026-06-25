# Session Watcher Design Spec

**Date:** 2026-06-22  
**Status:** Approved  
**Scope:** agbox CLI pivot from hook-based prompt capture to automatic session-level ingestion

> 2026-06-23 update: the current beta keeps the session watcher pivot, but managed
> workflow hooks are back in scope for in-agent suggestions and skill-file
> acknowledgement. Hook-based prompt capture remains out of scope. Sections below
> that say hook commands are removed are superseded by the beta aha loop plan in
> `docs/plans/2026-06-23-001-feat-beta-aha-loop-plan.md`.
>
> 2026-06-25 update: the primary product surface is now Recorded Workflows.
> `agbox inbox` shows workflow cards and replay plans. Prompt-submit hooks for
> supported agents can suggest apply-once replay for the current request, while
> Stop hooks can separately ask whether to save an applied workflow for future
> use through native `SKILL.md` acknowledgement. Replay remains instruction-only:
> agbox does not re-run prior commands or create persistent skills without
> explicit approval.

---

## Problem

agbox currently captures user corrections via reversible hooks (`connect` / `hook`). This only stores the correction text, not the causal context (what the agent did immediately before the user corrected it). The product promise on the landing page mentions agent sessions, but implementation is hook-centric.

The core product loop should be:

```text
agent action -> user correction -> repeated pattern -> recorded workflow -> saved workflow
```

This causal chain requires session-level analysis, not prompt hooks.

---

## Goals

1. Automatically ingest corrections from Claude Code, Codex, and Cursor session files after `npm install`.
2. Make `agbox inbox` the primary user-facing interface, with `agbox review` as the deeper TUI.
3. Show causal evidence (agent action → user correction) with summary + drill-down.
4. Integrate export into the review TUI.
5. Remove hook-based capture from v1.
6. Preserve privacy: no raw transcript storage in the agbox DB.

## Non-Goals (v1)

- Windows/Linux watcher support (macOS arm64 only, matching current npm package).
- Push notifications or macOS Notification Center integration.
- Remote/cloud sync or team features.
- Hook-based capture as fallback.
- Storing full session transcripts in SQLite.

---

## UX Decisions (Approved)

| Decision | Choice |
|----------|--------|
| Notification model | Quiet accumulation — no push alerts; user pulls via `agbox review` |
| Primary interface | `agbox inbox` Recorded Workflow cards; `agbox review` TUI for drill-down |
| Evidence display | Summary causal chains + `enter` drill-down per occurrence |
| Export flow | Integrated in TUI via `e` → target picker |
| Installation | `npm install` postinstall installs watcher; `agbox init` for repair |
| v1 session sources | Claude Code + Codex + Cursor (build order: Claude → Codex → Cursor) |
| Store location | Global `~/.agbox/agbox.db` with project filter in review TUI |
| Hook commands | Managed workflow hooks install replay, save-for-future, and acknowledgement entrypoints |

---

## User Journey

```text
npm install -g @agboxhq/cli
  → postinstall registers watcher + discovers session sources
  → prints: "watcher installed · run agbox doctor to verify"

(user codes normally; agbox collects quietly)

agbox inbox
  → Recorded Workflow cards
  → when it applies + replay plan + evidence + safety note
  → reject or snooze noisy workflows

prompt-submit hook
  → match the current user prompt to a Recorded Workflow
  → ask whether to apply the replay plan for this request only
  → if yes: agent follows instruction-only replay and runs agbox apply

stop hook
  → if a workflow was applied once, ask whether to save for future
  → if yes: create native SKILL.md with agbox_candidate_id
  → acknowledgement marks the workflow saved for future

agbox doctor          # only when troubleshooting
agbox init            # reinstall/repair watcher (not required for npm users)
```

### Review TUI Keymap (v1)

| Key | Action |
|-----|--------|
| `j` / `k` | Move between Recorded Workflows |
| `enter` | Drill into selected occurrence |
| `esc` | Back to summary |
| `a` → `y` | Approve Recorded Workflow |
| `x` → `y` | Reject Recorded Workflow |
| `e` → target | Export approved workflow |
| `f` | Toggle project filter (current / all) |
| `r` | Refresh data |
| `?` | Toggle help |
| `q` | Quit |

---

## Architecture

### Recommended Approach

**File watcher + incremental parse** with polling fallback.

```text
session source files (Claude / Codex / Cursor)
        ↓
LaunchAgent → agbox watch (internal)
        ↓
fswatch on known directories
        ↓
adapter.ParseDelta(cursor) per agent
        ↓
canonical SessionTurn + AgentAction + Correction
        ↓
correction detector
        ↓
SQLite (~/.agbox/agbox.db)
        ↓
scan/cluster -> Recorded Workflows
        ↓
review TUI (evidence + export)
```

Polling fallback runs every 5 minutes if fswatch misses an event or watcher restarts.

### Installation Flow (npm postinstall)

```text
postinstall.js
  → verify darwin/arm64
  → chmod binary
  → unless AGBOX_SKIP_WATCHER=1:
      → run agbox init --quiet
          → discover sources
          → install LaunchAgent plist
          → start watcher
  → print one-line status
```

`agbox init` remains available for manual repair and non-npm installs (`go install`).

---

## Data Model

### Storage Principle

- Session files remain the local source of truth on disk.
- agbox DB stores: source path, source hash, parse cursor, redacted excerpts, normalized correction text, structural metadata.
- No full transcript blobs in SQLite.

### Global Store

Default path: `~/.agbox/agbox.db` (unchanged from current implementation).

Project-local `.agbox/` in repos is reserved for export manifest/config only (not the primary DB).

### New Tables (v2 schema migration)

```sql
CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  agent TEXT NOT NULL,
  project TEXT NOT NULL,
  source_path TEXT NOT NULL,
  source_hash TEXT NOT NULL,
  started_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE turns (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  turn_index INTEGER NOT NULL,
  role TEXT NOT NULL,           -- agent | user
  event_type TEXT NOT NULL,     -- message | tool | command
  created_at TEXT NOT NULL,
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE TABLE actions (
  id TEXT PRIMARY KEY,
  turn_id TEXT NOT NULL,
  tool_name TEXT NOT NULL,
  command TEXT NOT NULL,
  file_path TEXT NOT NULL,
  excerpt TEXT NOT NULL,
  FOREIGN KEY(turn_id) REFERENCES turns(id) ON DELETE CASCADE
);

CREATE TABLE corrections (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  hash TEXT NOT NULL,
  normalized TEXT NOT NULL,
  excerpt TEXT NOT NULL,
  agent TEXT NOT NULL,
  project TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE,
  FOREIGN KEY(turn_id) REFERENCES turns(id) ON DELETE CASCADE,
  FOREIGN KEY(action_id) REFERENCES actions(id) ON DELETE CASCADE
);

CREATE TABLE source_cursors (
  source_path TEXT PRIMARY KEY,
  agent TEXT NOT NULL,
  last_offset INTEGER NOT NULL,
  last_hash TEXT NOT NULL,
  last_synced_at TEXT NOT NULL
);

CREATE TABLE candidate_corrections (
  candidate_id TEXT NOT NULL,
  correction_id TEXT NOT NULL,
  PRIMARY KEY(candidate_id, correction_id),
  FOREIGN KEY(candidate_id) REFERENCES candidates(id) ON DELETE CASCADE,
  FOREIGN KEY(correction_id) REFERENCES corrections(id) ON DELETE CASCADE
);
```

### `events` Table

Keep `events` table for manual capture and recurring prompt-pattern detection. New correction ingestion writes to `corrections`, while `scan` reads both correction rows and eligible event rows in the same run. Candidates carry a source kind (`correction` or `prompt_pattern`) so evidence and proposal copy can describe the signal honestly.

---

## Session Adapters

### Interface

```go
type Adapter interface {
    Agent() string
    DiscoverSources() ([]Source, error)
    ParseDelta(source Source, cursor Cursor) (ParseResult, error)
}
```

### Build Order

1. **Claude Code** — parse project jsonl under `~/.claude/projects/`
2. **Codex** — parse session files under `~/.codex/`
3. **Cursor** — parse composer/chat logs under Cursor app data; adapter interface first, implementation last due to format instability

### Correction Detector

A correction is recorded when:

1. An agent turn contains an action (tool call, terminal command, file write).
2. The next user turn is a corrective message (heuristic + normalization).
3. The pair is deduplicated by `(action_fingerprint, normalized_correction)`.

Example:

```text
turn 3  agent  tool:run_terminal_cmd  command:"npm install"
turn 4  user   "use bun, not npm"
→ correction linked to action turn 3
```

Clustering fingerprint: `sha256(normalized_correction + "|" + action_fingerprint)`.

---

## Evidence Model

### EvidenceCard (extended)

```go
type Occurrence struct {
    ID              string
    SessionID       string
    CreatedAt       time.Time
    AgentAction     string   // one-line summary
    UserCorrection  string   // redacted excerpt
    DrillDown       []DrillStep
}

type EvidenceCard struct {
    Candidate   Candidate
    Occurrences []Occurrence
    Projects    []string
    Agents      []string
    Reason      string
    Privacy     string
}
```

### TUI Views

**Summary view:** top 5 occurrences as `agent action → user correction` lines.  
**Drill-down view:** full turn chain for one occurrence (redacted excerpts only).

---

## Watcher

### LaunchAgent

Path: `~/Library/LaunchAgents/com.agboxhq.watcher.plist`

Runs: `agbox watch` (internal command, not documented in primary help)

Logs: `~/.agbox/watcher.log`

### Doctor Output (target)

```text
store:       OK ~/.agbox/agbox.db
watcher:     running (pid 12345)
last sync:   2m ago
sources:
  claude     ~/.claude/projects/.../session.jsonl   synced
  codex      ~/.codex/sessions/...                  synced
  cursor     ~/Library/Application Support/Cursor/...  pending
corrections: 142
recorded workflows: 8
exports:     3
```

---

## Command Changes

### Add / Change

| Command | Role |
|---------|------|
| `agbox init` | Install/repair watcher + register sources |
| `agbox watch` | Internal daemon entry (LaunchAgent target) |
| `agbox status` | Short status: watcher, last sync, candidate count |
| `agbox sources` | List discovered session source paths |
| `agbox sync --once` | Debug/recovery: force one ingestion pass |
| `agbox inbox` | Primary UX: Recorded Workflow cards and replay plans |
| `agbox review` | Deeper TUI drill-down for evidence, approval, and export |
| `agbox connect` | Install managed workflow hooks |
| `agbox disconnect` | Remove managed workflow hooks |
| `agbox hook` | Managed hook entrypoints for propose, replay, save, and acknowledge |

### Keep

`capture`, `scan`, `discover`, `evidence`, `apply`, `approve`, `reject`, `snooze`, `accept`, `export`, `impact`, `audit`, `doctor`, `demo`, `debug-bundle`, `repair`, `manifest`, `compile`

`capture` remains for manual testing only.

---

## Privacy

- Parse session files in place; never copy full transcript into DB.
- Store redacted excerpts (max 240 chars per field, matching current `privacy.Excerpt`).
- Hash normalized correction text for deduplication.
- `agbox audit` and `debug-bundle` must not include raw session content.
- Opt-out: `AGBOX_SKIP_WATCHER=1` on install.

---

## Documentation Updates

### agbox README

- Quick start: `npm install -g @agboxhq/cli` -> `agbox inbox`
- Document managed workflow hooks and `AGBOX_SKIP_CONNECT`.
- Fix store description: global `~/.agbox/agbox.db`, not project `.agbox/index.db`.
- Update architecture diagram: session watcher -> scan -> replay once -> save for future.

### npm README

- Document postinstall watcher behavior and opt-out env var.

### agbox-landing-page

- Hero copy: remove "through hooks"
- Demo component: replace hook animation with session watcher + review TUI flow.

---

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Cursor session format changes | Adapter interface + graceful skip in doctor |
| postinstall LaunchAgent fails silently | doctor shows watcher state; init is idempotent repair |
| Global DB mixes projects | review TUI defaults to cwd project filter |
| Session parse false positives | require agent action before user correction; min 2 occurrences for candidate |
| Go module path mismatch (`hippoom` vs `qyinm`) | align in separate chore PR if needed |

---

## Success Criteria

1. `npm install -g @agboxhq/cli` installs watcher without user action.
2. After repeated prompts or corrections, `agbox inbox` shows a Recorded Workflow with evidence and a replay plan.
3. Prompt-submit hooks suggest apply-once replay only for matching current prompts.
4. `agbox doctor` reports watcher health and source sync status.
5. `go test ./...` passes with watcher and managed hook packages intact.
6. CLI help and README describe `apply once` separately from save-for-future SKILL creation.

---

## Self-Review Checklist

- [x] All approved UX decisions captured
- [x] Store location matches code (`~/.agbox/agbox.db`) and supersedes README `.agbox/index.db` claim
- [x] Managed hook scope explicit
- [x] Privacy constraints explicit (no raw transcript in DB)
- [x] v1 platform scope explicit (macOS arm64)
- [x] Adapter build order explicit (Claude → Codex → Cursor)
- [x] No TBD placeholders
