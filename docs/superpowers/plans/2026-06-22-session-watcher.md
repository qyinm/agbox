# Session Watcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace hook-based prompt capture with automatic session ingestion from Claude/Codex/Cursor, centered on an enhanced `agbox review` TUI.

**Architecture:** macOS LaunchAgent runs `agbox watch`, which fswatches known session directories. Per-agent adapters parse file deltas into canonical turns/actions/corrections stored in global `~/.agbox/agbox.db`. Scan clusters corrections into candidates; review TUI shows causal evidence with drill-down and inline export.

**Tech Stack:** Go 1.22+, SQLite (mattn/go-sqlite3), Bubble Tea v2, macOS LaunchAgent, Node postinstall script.

**Spec:** `docs/superpowers/specs/2026-06-22-session-watcher-design.md`

---

## File Map

| File | Responsibility |
|------|----------------|
| `internal/model/session.go` | Session, Turn, Action, Correction, Occurrence types |
| `internal/session/adapter.go` | Adapter interface, Source, Cursor, ParseResult |
| `internal/session/registry.go` | Register Claude/Codex/Cursor adapters |
| `internal/session/claude/adapter.go` | Claude Code jsonl parser |
| `internal/session/codex/adapter.go` | Codex session parser |
| `internal/session/cursor/adapter.go` | Cursor session parser |
| `internal/session/detect.go` | Agent-action → user-correction detector |
| `internal/session/ingest.go` | Orchestrates adapter parse + store write |
| `internal/store/migrate_v2.go` | v2 schema tables |
| `internal/store/corrections.go` | CRUD for sessions/turns/actions/corrections/cursors |
| `internal/scan/scan.go` | Cluster corrections (not flat events) |
| `internal/evidence/evidence.go` | Build Occurrence causal chains |
| `internal/watcher/watcher.go` | fswatch loop + polling fallback |
| `internal/watcher/install.go` | LaunchAgent plist install/start/stop |
| `internal/cli/init.go` | `agbox init` command |
| `internal/cli/watch.go` | `agbox watch` internal daemon |
| `internal/cli/cli.go` | Wire commands; remove hook/connect |
| `internal/tui/review.go` | Drill-down, export, project filter |
| `internal/doctor/doctor.go` | Watcher + source health |
| `npm/cli/scripts/postinstall.js` | Call `agbox init --quiet` |
| `internal/connect/*` | **DELETE** |

---

### Task 1: Session Domain Types

**Files:**
- Create: `internal/model/session.go`
- Test: `internal/model/session_test.go`

- [ ] **Step 1: Write the failing test**

```go
package model_test

import (
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/model"
)

func TestOccurrenceSummaryLine(t *testing.T) {
	occ := model.Occurrence{
		AgentAction:    "ran `npm install`",
		UserCorrection: "use bun, not npm",
	}
	got := occ.SummaryLine()
	want := "ran `npm install`  →  use bun, not npm"
	if got != want {
		t.Fatalf("SummaryLine() = %q, want %q", got, want)
	}
}

func TestDrillStepFormat(t *testing.T) {
	step := model.DrillStep{
		TurnIndex: 3,
		Role:      "agent",
		Summary:   "Ran: npm install",
		CreatedAt: time.Date(2026, 6, 20, 10, 0, 0, 0, time.UTC),
	}
	got := step.Format()
	if got == "" {
		t.Fatal("Format() returned empty string")
	}
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd agbox && go test ./internal/model/... -run TestOccurrence -v`  
Expected: FAIL — `Occurrence` / `DrillStep` undefined

- [ ] **Step 3: Implement types**

```go
package model

import (
	"fmt"
	"time"
)

type Session struct {
	ID         string
	Agent      string
	Project    string
	SourcePath string
	SourceHash string
	StartedAt  time.Time
	UpdatedAt  time.Time
}

type Turn struct {
	ID        string
	SessionID string
	TurnIndex int
	Role      string // agent | user
	EventType string // message | tool | command
	CreatedAt time.Time
}

type Action struct {
	ID       string
	TurnID   string
	ToolName string
	Command  string
	FilePath string
	Excerpt  string
}

type Correction struct {
	ID         string
	SessionID  string
	TurnID     string
	ActionID   string
	Hash       string
	Normalized string
	Excerpt    string
	Agent      string
	Project    string
	CreatedAt  time.Time
}

type DrillStep struct {
	TurnIndex int
	Role      string
	Summary   string
	CreatedAt time.Time
}

func (s DrillStep) Format() string {
	return fmt.Sprintf("turn %d  %s  %s", s.TurnIndex, s.Role, s.Summary)
}

type Occurrence struct {
	ID             string
	SessionID      string
	CreatedAt      time.Time
	AgentAction    string
	UserCorrection string
	DrillDown      []DrillStep
}

func (o Occurrence) SummaryLine() string {
	return o.AgentAction + "  →  " + o.UserCorrection
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `go test ./internal/model/... -v`  
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add internal/model/session.go internal/model/session_test.go
git commit -m "feat: add session domain types for correction evidence"
```

---

### Task 2: v2 Schema Migration

**Files:**
- Create: `internal/store/migrate_v2.go`
- Modify: `internal/store/store.go` (call v2 migrate from `migrate()`)
- Test: `internal/store/migrate_v2_test.go`

- [ ] **Step 1: Write failing test**

```go
package store_test

import (
	"path/filepath"
	"testing"

	"github.com/hippoom/agbox/internal/store"
)

func TestMigrateV2CreatesCorrectionTables(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	for _, table := range []string{"sessions", "turns", "actions", "corrections", "source_cursors", "candidate_corrections"} {
		if !s.TableExists(table) {
			t.Fatalf("table %q not created", table)
		}
	}
}
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `go test ./internal/store/... -run TestMigrateV2 -v`

- [ ] **Step 3: Add `TableExists` helper and v2 migration**

In `internal/store/store.go`, add:

```go
func (s *Store) TableExists(name string) bool {
	var n int
	_ = s.db.QueryRow(`SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?`, name).Scan(&n)
	return n == 1
}
```

Create `internal/store/migrate_v2.go` with all CREATE TABLE statements from the design spec. Call `migrateV2(s.db)` at end of `migrate()`.

- [ ] **Step 4: Run test — expect PASS**

- [ ] **Step 5: Commit**

```bash
git add internal/store/migrate_v2.go internal/store/migrate_v2_test.go internal/store/store.go
git commit -m "feat: add v2 schema for session corrections"
```

---

### Task 3: Adapter Interface + Registry

**Files:**
- Create: `internal/session/adapter.go`
- Create: `internal/session/registry.go`
- Test: `internal/session/registry_test.go`

- [ ] **Step 1: Write failing test**

```go
func TestRegistryListsAgents(t *testing.T) {
	agents := session.AgentNames()
	if len(agents) < 1 {
		t.Fatal("expected at least one registered adapter")
	}
}
```

- [ ] **Step 2: Implement interface**

```go
package session

type Source struct {
	Agent      string
	Path       string
	Project    string
}

type Cursor struct {
	SourcePath string
	LastOffset int64
	LastHash   string
}

type ParseResult struct {
	Session    model.Session
	Turns      []model.Turn
	Actions    []model.Action
	Corrections []model.Correction
	NewOffset  int64
	NewHash    string
}

type Adapter interface {
	Agent() string
	DiscoverSources() ([]Source, error)
	ParseDelta(src Source, cur Cursor) (ParseResult, error)
}
```

`registry.go` holds `[]Adapter` and exposes `All()`, `ByAgent(name)`, `AgentNames()`.

- [ ] **Step 3: Run tests, commit**

```bash
git commit -m "feat: add session adapter interface and registry"
```

---

### Task 4: Claude Code Adapter

**Files:**
- Create: `internal/session/claude/adapter.go`
- Create: `internal/session/claude/testdata/sample.jsonl`
- Test: `internal/session/claude/adapter_test.go`

- [ ] **Step 1: Create fixture jsonl**

`testdata/sample.jsonl` — two agent tool-use lines followed by one user correction:

```jsonl
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"run_terminal_cmd","input":{"command":"npm install"}}]}}
{"type":"user","message":{"content":[{"type":"text","text":"use bun, not npm"}]}}
```

- [ ] **Step 2: Write failing test**

```go
func TestParseDeltaDetectsCorrection(t *testing.T) {
	adapter := claude.New()
	src := session.Source{Agent: "claude", Path: "testdata/sample.jsonl", Project: "demo"}
	result, err := adapter.ParseDelta(src, session.Cursor{})
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Corrections) != 1 {
		t.Fatalf("corrections = %d, want 1", len(result.Corrections))
	}
	if result.Corrections[0].Excerpt == "" {
		t.Fatal("expected redacted excerpt")
	}
}
```

- [ ] **Step 3: Implement parser**

Parse jsonl line-by-line from `cursor.LastOffset`. Map assistant tool_use → `Action`. Map following user text → `Correction` linked to prior action. Use `privacy.Redact`, `privacy.NormalizeSignal`, `privacy.Excerpt`.

- [ ] **Step 4: Register in `registry.go`**

- [ ] **Step 5: Run tests, commit**

```bash
git commit -m "feat: add Claude Code session adapter"
```

---

### Task 5: Correction Detector (shared heuristics)

**Files:**
- Create: `internal/session/detect.go`
- Test: `internal/session/detect_test.go`

- [ ] **Step 1: Write failing test for pairing logic**

```go
func TestPairActionAndCorrection(t *testing.T) {
	turns := []model.Turn{
		{TurnIndex: 1, Role: "agent", EventType: "tool"},
		{TurnIndex: 2, Role: "user", EventType: "message"},
	}
	actions := []model.Action{{TurnID: turns[0].ID, Command: "npm install", Excerpt: "npm install"}}
	pairs := session.PairCorrections(turns, actions, map[string]string{turns[1].ID: "use bun, not npm"})
	if len(pairs) != 1 {
		t.Fatalf("pairs = %d, want 1", len(pairs))
	}
}
```

- [ ] **Step 2: Implement `PairCorrections`**

Skip user messages that don't look corrective (too short, no imperative verbs, matches agent output). Reuse existing `privacy` helpers.

- [ ] **Step 3: Run tests, commit**

---

### Task 6: Store Methods + Ingest Orchestrator

**Files:**
- Create: `internal/store/corrections.go`
- Create: `internal/session/ingest.go`
- Test: `internal/session/ingest_test.go`

- [ ] **Step 1: Write failing ingest test**

Uses temp dir + sample jsonl + in-memory store. Calls `session.IngestOnce(store, "claude")`. Asserts `corrections` count > 0 and `source_cursors` updated.

- [ ] **Step 2: Implement store insert/upsert methods**

`UpsertSession`, `InsertTurns`, `InsertActions`, `InsertCorrection`, `GetCursor`, `UpsertCursor`, `ListCorrections`, `CorrectionsForCandidate`.

- [ ] **Step 3: Implement `IngestOnce` and `IngestAll`**

For each adapter: discover sources → read cursor → parse delta → store → update cursor.

- [ ] **Step 4: Run tests, commit**

```bash
git commit -m "feat: add session ingest pipeline and correction store"
```

---

### Task 7: Scan Clustering on Corrections

**Files:**
- Modify: `internal/scan/scan.go`
- Modify: `internal/scan/scan_test.go`

- [ ] **Step 1: Write failing test**

Seed DB with 3 corrections same normalized text + same action fingerprint. Run `scan.Run(s, 2)`. Expect 1 candidate with `EventCount >= 2`.

- [ ] **Step 2: Update scan to read `corrections` table**

Fingerprint: `sha256(normalized + "|" + actionFingerprint(action))`. Link via `candidate_corrections` table. Fall back to legacy `events` if no corrections exist (keeps demo working).

- [ ] **Step 3: Run all tests**

Run: `go test ./internal/scan/... -v`

- [ ] **Step 4: Commit**

```bash
git commit -m "feat: cluster session corrections into candidates"
```

---

### Task 8: Causal Evidence Builder

**Files:**
- Modify: `internal/evidence/evidence.go`
- Create: `internal/evidence/evidence_test.go`

- [ ] **Step 1: Write failing test**

Seed correction with linked action/turns. `evidence.Build(s, candidateID)` returns `Occurrences` with `SummaryLine()` and `DrillDown` length >= 2.

- [ ] **Step 2: Implement occurrence builder**

Load corrections for candidate → join action + user turn → build `DrillStep` chain (agent action, user correction, optional agent fix).

- [ ] **Step 3: Run tests, commit**

---

### Task 9: Watcher + LaunchAgent Install

**Files:**
- Create: `internal/watcher/install.go`
- Create: `internal/watcher/watcher.go`
- Create: `internal/cli/init.go`
- Create: `internal/cli/watch.go`
- Test: `internal/watcher/install_test.go`, `internal/watcher/watcher_test.go`

- [ ] **Step 1: Write failing install test**

Uses temp home. `watcher.Install(home, agboxBin)` writes plist to `~/Library/LaunchAgents/com.agboxhq.watcher.plist`. Assert file exists and contains `agbox watch`.

- [ ] **Step 2: Implement install.go**

Functions: `Install`, `Uninstall`, `IsRunning`, `Status`. Plist label `com.agboxhq.watcher`. RunAtLoad true. KeepAlive false (watch process loops internally).

- [ ] **Step 3: Implement watcher.go**

`Run(ctx, store, interval)` — initial `session.IngestAll`, then fswatch session dirs (use `github.com/fsnotify/fsnotify` or shell `fswatch` if already a dep; prefer fsnotify to avoid external binary). Polling fallback every 5m.

- [ ] **Step 4: Wire `agbox init` and `agbox watch`**

`init` flags: `--quiet`. Discovers sources, installs LaunchAgent, starts watcher.  
`watch` — daemon entry, not in primary help text.

- [ ] **Step 5: Add `agbox status` and `agbox sources`**

`status`: watcher running, last sync, correction/candidate counts.  
`sources`: list discovered paths per agent.

- [ ] **Step 6: Run tests, commit**

```bash
git commit -m "feat: add session watcher and LaunchAgent install"
```

---

### Task 10: Review TUI — Drill-down, Filter, Export

**Files:**
- Modify: `internal/tui/review.go`
- Modify: `internal/tui/review_test.go`
- Modify: `internal/cli/cli.go` (wire export from TUI)

- [ ] **Step 1: Write failing tests for new view states**

Test `ReviewModel` transitions: `viewSummary` → `viewDrillDown` on `enter`, back on `esc`. Test `projectFilter` toggles candidate list.

- [ ] **Step 2: Add view state enum**

```go
type viewMode int
const (
	viewList viewMode = iota
	viewDrillDown
	viewExportTarget
)
```

- [ ] **Step 3: Render occurrences in evidence panel**

Show up to 5 `occ.SummaryLine()` entries. Highlight selected occurrence.

- [ ] **Step 4: Implement drill-down panel**

On `enter`, show `DrillStep.Format()` lines for selected occurrence.

- [ ] **Step 5: Implement export flow**

On `e` (approved candidates only), show target picker: `1 agents-md  2 claude  3 cursor  4 cline`. On selection, call existing `export` package. Print result inline.

- [ ] **Step 6: Implement `f` project filter**

Default: filter candidates to `defaultProject()` cwd. Toggle shows all.

- [ ] **Step 7: Run tests, commit**

```bash
git commit -m "feat: enhance review TUI with drill-down, filter, and export"
```

---

### Task 11: Remove Hook Path

**Files:**
- Delete: `internal/connect/connect.go`, `internal/connect/connect_test.go`
- Modify: `internal/cli/cli.go`
- Modify: `internal/cli/cli_test.go`
- Modify: `internal/doctor/doctor.go`, `internal/doctor/doctor_test.go`

- [ ] **Step 1: Remove switch cases** for `connect`, `disconnect`, `hook` from `cli.go`

- [ ] **Step 2: Delete connect package and update doctor**

Doctor reports watcher/sources instead of hook status.

- [ ] **Step 3: Update cli tests**

Remove hook/connect tests. Add test that `agbox hook` returns unknown command.

- [ ] **Step 4: Run full test suite**

Run: `go test ./...`  
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git commit -m "refactor: remove hook-based capture path"
```

---

### Task 12: npm postinstall Watcher Setup

**Files:**
- Modify: `npm/cli/scripts/postinstall.js`

- [ ] **Step 1: Update postinstall**

```js
if (process.env.AGBOX_SKIP_WATCHER === "1") {
  console.log("agbox: watcher install skipped (AGBOX_SKIP_WATCHER=1)");
  process.exit(0);
}
const { execFileSync } = require("node:child_process");
try {
  execFileSync(executable, ["init", "--quiet"], { stdio: "pipe" });
  console.log("agbox: watcher installed · run `agbox doctor` to verify");
} catch (err) {
  console.error("agbox: watcher install failed — run `agbox init` manually");
}
```

- [ ] **Step 2: Manual smoke test**

Run: `node npm/cli/scripts/postinstall.js` (with built binary present)  
Expected: watcher installed message or clear failure message

- [ ] **Step 3: Commit**

```bash
git commit -m "feat: install session watcher via npm postinstall"
```

---

### Task 13: Codex + Cursor Adapters

**Files:**
- Create: `internal/session/codex/adapter.go` + testdata + tests
- Create: `internal/session/cursor/adapter.go` + testdata + tests

- [ ] **Step 1: Codex adapter** — mirror Claude adapter pattern for `~/.codex/` session files. Write fixture-based test first.

- [ ] **Step 2: Cursor adapter** — implement best-effort parser; if format unknown, `DiscoverSources` returns paths but `ParseDelta` returns empty with no error. Doctor shows `pending (format)`.

- [ ] **Step 3: Register both in registry**

- [ ] **Step 4: Run tests, commit**

```bash
git commit -m "feat: add Codex and Cursor session adapters"
```

---

### Task 14: Documentation + Landing Page

**Files:**
- Modify: `README.md`
- Modify: `npm/cli/README.md`
- Modify: `agbox-landing-page/components/hero-section.tsx`
- Modify: `agbox-landing-page/components/claude-code-live-session-demo.tsx`

- [ ] **Step 1: Update agbox README**

Quick start:

```bash
npm install -g @agboxhq/cli
agbox review
```

Remove `connect`/`hook` sections. Fix store path to `~/.agbox/agbox.db`. Update architecture diagram.

- [ ] **Step 2: Update landing hero**

Replace "through hooks" with "watches your agent sessions locally".

- [ ] **Step 3: Update demo component**

Replace hook install animation with watcher status + `agbox review` evidence view.

- [ ] **Step 4: Commit both repos**

```bash
# agbox
git commit -m "docs: update README for session watcher UX"

# agbox-landing-page
git commit -m "docs: update landing copy for session watcher"
```

---

### Task 15: End-to-End Verification

- [ ] **Step 1: Run full Go test suite**

```bash
cd agbox && go test ./...
```

Expected: all PASS

- [ ] **Step 2: Run demo with session fixture**

Add `agbox demo` path that seeds from `testdata/sample.jsonl` via ingest, then opens review data. Verify causal evidence appears.

- [ ] **Step 3: Run doctor**

```bash
go run ./cmd/agbox doctor
```

Expected: watcher + sources + correction counts

- [ ] **Step 4: Final commit**

```bash
git commit -m "test: add session ingest demo path and verify e2e"
```

---

## Plan Self-Review

### Spec Coverage

| Spec Requirement | Task |
|-----------------|------|
| Quiet pull UX | Task 10 (review as primary), no notification code |
| review TUI main | Task 10 |
| Evidence drill-down | Tasks 1, 8, 10 |
| Export in TUI | Task 10 |
| npm postinstall | Task 12 |
| Claude/Codex/Cursor | Tasks 4, 13 |
| Global store + filter | Tasks 2, 6, 10 |
| Hook removal | Task 11 |
| Watcher + doctor | Tasks 8, 9, 11 |
| Privacy (no raw transcript) | Tasks 4, 6 (privacy helpers) |
| README/landing updates | Task 14 |

### Dependency Order

```text
Task 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10 → 11 → 12 → 13 → 14 → 15
                              ↘ 13 can parallel after 3
```

### Known Gaps (acceptable for v1)

- Cursor adapter may parse zero corrections until format is confirmed — doctor shows `pending`.
- `agbox sync --once` — add in Task 9 alongside `status`/`sources` (thin wrapper over `IngestAll`).
- Legacy `events` table kept for demo backward compat until Task 15 updates demo seed.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-22-session-watcher.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks
2. **Inline Execution** — implement task-by-task in this session with checkpoints

Which approach?