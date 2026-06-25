---
title: Recorded Workflow Replay Lifecycle Guardrails
date: 2026-06-25
category: workflow-issues
module: recorded_workflow_replay
problem_type: workflow_issue
component: development_workflow
severity: medium
applies_when:
  - Reviewing or changing recorded workflow replay lifecycle states
  - Adding prompt-submit replay or save-for-future hooks
  - Persisting replay applications or candidate state transitions
  - Rendering recorded workflow card text from stored evidence
  - Growing workflow command tests near file-size budgets
symptoms:
  - applied_once and save_suggested workflows stopped being prompt-replay eligible before the user saved, rejected, or snoozed them
  - save-for-future prompts could be selected from replay applications in a different project
  - replay application and save-suggested updates lacked expected-state guards against stale transitions
  - prompt-submit replay hooks ran stale sync on every prompt and save prompt state-update failures could be hidden
  - workflow card display text could preserve terminal control sequences
root_cause: logic_error
resolution_type: code_fix
related_components:
  - assistant
  - tooling
  - testing_framework
tags:
  - recorded-workflow-replay
  - candidate-lifecycle
  - prompt-submit-hooks
  - save-for-future
  - state-guards
  - project-scoping
  - terminal-sanitization
  - cli-test-split
---

# Recorded Workflow Replay Lifecycle Guardrails

## Context

Recorded Workflow replay has two separate user decisions that must not collapse into one lifecycle transition:

- Apply this replay once for the current request.
- Save this workflow for future automatic use.

During review of `feat/recorded-workflow-replay`, several fixes made that lifecycle durable across storage, CLI hooks, prompt selection, display, and tests. The relevant commits were `83d366b`, `742816c`, `8b676e1`, and `93b54bf`; verification passed with `go test ./...` and `go vet ./...`.

The core guardrail is that `applied_once` and `save_suggested` are intermediate replay lifecycle states, not final states. A workflow in either state should remain eligible for prompt replay until the user explicitly saves, rejects, or snoozes it. At the same time, state transitions out of those states need expected-state guards so stale hooks or scan merges cannot overwrite a newer user decision.

No relevant prior session-history findings were found for this specific fix.

## Guidance

Treat replay lifecycle states as explicit, user-driven states. Do not infer "done" from `applied_once` or `save_suggested`.

`SelectForPrompt` in `internal/propose/propose.go` deliberately includes all prompt-replay-eligible lifecycle states:

```go
candidates, err := s.ListCandidatesByState(
	model.CandidateProposalReady,
	model.CandidateAppliedOnce,
	model.CandidateSaveSuggested,
)
```

That means a workflow can be replayed again after a one-time application or after a save prompt has been shown, as long as it still matches the current prompt and project. `TestSelectForPromptIncludesPreviouslyAppliedReplayStates` protects this behavior and verifies that delivering replay instructions does not mutate `applied_once` or `save_suggested` back to `proposed`.

Scope save-for-future prompts to replay applications, not just candidates. `SelectForSaveForFuture` starts from `CandidateAppliedOnce`, then requires both the current project and a recorded replay application for that candidate/project:

```go
if project != "" && !candidateMatchesProject(s, c.ID, project) {
	continue
}
app, err := s.LatestReplayApplication(c.ID, project)
if err != nil {
	continue
}
```

This distinction matters for cross-project workflows. Candidate evidence answers "has this workflow been observed?" A replay application answers "did this user/project apply this workflow?" Save prompts need the second answer. `TestSelectForSaveForFutureRequiresApplicationInProject` is the important regression test.

Use expected-state updates for replay lifecycle transitions. `UpdateCandidateMetaIfState` updates only when the row is still in the expected state:

```sql
UPDATE candidates
SET ...
WHERE id = ? AND state = ?
```

Use this helper for hook-driven state transitions such as `proposal_ready -> proposed` and `applied_once -> save_suggested`. This prevents stale hook output from clobbering a user decision that happened between selection and persistence. `TestUpdateCandidateMetaIfStateRequiresExpectedState` captures the expected behavior: a stale expected state returns `updated=false` and leaves the candidate unchanged.

Record replay applications transactionally. `RecordReplayApplication` inserts the replay application and, when allowed, marks the candidate `applied_once` in the same transaction. It reads the current state, inserts the application row, and updates with the current state in the `WHERE` clause. If the row changed concurrently, the method returns an error rather than silently losing the race.

Keep scan/upsert logic from erasing replay lifecycle states. `internal/propose/state/state.go` includes `CandidateAppliedOnce` and `CandidateSaveSuggested` in `IsFrozen`, so scan merges preserve them. The tests `TestMergeOnScanPreservesReplayLifecycleStates` and `TestUpsertCandidatePreservesAppliedOnceAndSaveSuggested` protect both the in-memory state merge and the store-level upsert path.

Keep prompt-submit replay hooks low latency. `runHookReplay` reads hook JSON, derives the project and prompt, and calls `SelectAndRenderForPrompt`. It does not call `syncBestEffortIfStale`; stale sync remains in proposal and save hooks, where it is not on every prompt-submit path:

```go
prompt := propose.PromptFromHook(hookData)
candidateID, payload, err := propose.SelectAndRenderForPrompt(s, agent, project, prompt)
```

`TestHookReplayDoesNotRunStaleSync` makes this explicit by replacing `syncBestEffortIfStale` with a function that fails the test if replay invokes it.

Surface save prompt persistence failures. `DeliverSaveSuggested` writes the prompt payload, then attempts to mark the candidate `save_suggested`. If persistence fails, it logs a warning and returns the error:

```go
if err := MarkSaveSuggested(s, candidateID); err != nil {
	if log != nil {
		_, _ = io.WriteString(log, "agbox: warning: save prompt "+candidateID+" delivered but state not updated: "+err.Error()+"\n")
	}
	return err
}
```

Do not hide this failure. A user who saw a save-for-future prompt while the state failed to persist is in an ambiguous workflow state. `TestDeliverSaveSuggestedReturnsPersistenceFailure` verifies both the warning and returned error.

Sanitize workflow card display text before it reaches terminal output. Display text can originate from recorded prompts, command excerpts, project names, or other local evidence. Strip ANSI CSI sequences, OSC controls such as clipboard escapes, and non-printing control runes before constructing card fields. Apply this at display construction boundaries such as `titleFromSlug`, `oneLine`, and evidence summaries, not only at the final renderer.

Keep workflow command tests close to the workflow surface. Use `internal/cli/workflow_commands_test.go` for replay, inbox, apply, save hook, and workflow card command behavior. Keep `internal/cli/cli_test.go` for general CLI execution, root help, telemetry, and command dispatch behavior. This keeps the large CLI suite navigable while preserving coverage around lifecycle regressions.

## Why This Matters

Replay hooks run in the user's prompt path. Any unnecessary sync, blocking I/O, or stale state transition is paid on every prompt-submit event, not only when the user opens a workflow screen.

Lifecycle mistakes create durable product bugs. If `applied_once` is treated as terminal, a useful recorded workflow disappears after the first replay. If `save_suggested` is treated as terminal before the user saves, rejects, or snoozes, the user can get stuck in a half-acknowledged state. If save prompts are selected from candidates rather than replay applications, a workflow applied in one project can ask to be saved in a different project where the user never approved it.

Expected-state guards matter because hooks and scans are asynchronous relative to user actions. A prompt hook may select a candidate, then the user may reject or save it before the hook persists a state transition. Unguarded writes would overwrite that newer decision. Guarded writes make stale work fail closed.

Save prompt persistence failures must be visible because the prompt itself changes user expectations. If the save prompt was delivered but `save_suggested` was not persisted, the next hook or inbox view may behave as though the prompt never happened. Returning the error lets the CLI surface the inconsistency instead of silently repeating or skipping steps.

Display sanitization is a safety and usability boundary. Evidence text and workflow names are derived from recorded content. Rendering them directly into terminal output without stripping control sequences risks broken output, hidden text, or terminal-side effects such as clipboard escape sequences.

Test organization matters because replay has many small contracts across CLI, propose, store, and workflow display code. Splitting workflow command tests keeps size pressure out of `cli_test.go` and makes it more likely that future engineers add focused regression coverage instead of weakening existing tests.

## When to Apply

- Changing Recorded Workflow replay, prompt hooks, save-for-future prompts, candidate lifecycle states, scan/upsert behavior, replay application storage, or workflow card rendering.
- Adding or renaming candidate states that affect prompt replay eligibility.
- Selecting save-for-future prompts from data that may span multiple projects.
- Writing hook or background-process code that transitions lifecycle state after rendering output or after doing selection work.
- Rendering terminal-facing display strings derived from recorded evidence, candidate names, prompts, command text, or project data.
- Adding workflow command coverage that would otherwise grow the generic CLI test file.

Use the prompt-replay eligibility rule whenever adding or renaming candidate states. Ask whether the state is truly terminal from the user's perspective. If the user has not saved, rejected, or snoozed the workflow, the state probably should remain replay eligible when the prompt and project match.

Use replay application scoping when a decision depends on the user having applied a replay. Candidate evidence answers whether the workflow has been observed. A replay application answers whether this user/project applied the workflow.

Use expected-state writes for any hook or background process that transitions lifecycle state after rendering output or after doing selection work. If the action depends on the candidate still being in a specific state, encode that state in the database update.

Skip stale sync from prompt-submit replay hooks unless a future design has a bounded, explicit latency budget and tests proving it does not run on every prompt. The replay hook should select from already-ingested data. Ingest freshness belongs in explicit sync, proposal, save, or watcher flows.

## Examples

### Keep Intermediate Replay States Eligible

Good prompt replay selection includes the intermediate lifecycle states:

```go
s.ListCandidatesByState(
	model.CandidateProposalReady,
	model.CandidateAppliedOnce,
	model.CandidateSaveSuggested,
)
```

Avoid narrowing this to only `proposal_ready`. That would remove workflows immediately after a one-time replay, even though the user has not made a terminal decision.

### Separate Replay Application From Candidate Evidence

For save-for-future prompts, require a same-project replay application:

```go
app, err := s.LatestReplayApplication(candidateID, project)
if err != nil {
	continue
}
```

Do not use candidate evidence alone for this decision. A multi-project candidate can be valid evidence but still lack a user-approved replay application in the current project.

### Guard Hook-Driven State Changes

Use expected-state transitions:

```go
updated, err := s.UpdateCandidateMetaIfState(
	candidateID,
	model.CandidateAppliedOnce,
	store.CandidateMetaUpdate{State: model.CandidateSaveSuggested},
)
```

Then treat `updated=false` as a stale transition. The candidate may have been rejected, saved, snoozed, or otherwise changed after the hook selected it.

### Keep Replay Hooks Fast

The replay hook should use the prompt already present in hook input:

```go
prompt := propose.PromptFromHook(hookData)
candidateID, payload, err := propose.SelectAndRenderForPrompt(s, agent, project, prompt)
```

Do not add stale sync here by default. The regression test should fail if replay invokes `syncBestEffortIfStale`.

### Return Save Prompt State Failures

If the save prompt is shown but the state update fails, return the persistence error:

```go
if err := MarkSaveSuggested(s, candidateID); err != nil {
	logWarning(...)
	return err
}
```

This is intentionally stricter than a best-effort display update because the user-visible prompt and the stored lifecycle must stay aligned.

### Sanitize Evidence Before Rendering

Treat evidence-derived display text as untrusted:

```go
func oneLine(value string) string {
	value = strings.Join(strings.Fields(sanitizeDisplayText(value)), " ")
	// ...
}
```

Keep tests with representative terminal controls, including ANSI colors and OSC 52 clipboard sequences, so future refactors cannot accidentally reintroduce raw escape output.

### Split Tests by Ownership

When adding workflow command coverage:

- Put `agbox inbox`, `agbox apply`, `agbox hook replay`, and `agbox hook save` behavior in `internal/cli/workflow_commands_test.go`.
- Keep root help, dispatch, telemetry, and broad CLI behavior in `internal/cli/cli_test.go`.
- Put lifecycle selection tests in `internal/propose/propose_test.go`.
- Put database transition and upsert preservation tests in `internal/store/migrate_v2_test.go` or the closest store test file.
- Put card rendering and sanitization tests in `internal/workflow/card_test.go`.

This makes the regression suite easier to extend when adding states or hooks, and it keeps file-size budgets from becoming a reason to avoid precise tests.

## Related

- `docs/brainstorms/2026-06-25-auto-workflow-record-replay-requirements.md` - product requirements for automatic workflow recording, instruction-only replay, apply once, explicit save/auto-apply confirmation, inbox management, and lifecycle vocabulary.
- `docs/plans/2026-06-25-003-feat-recorded-workflow-replay-plan.md` - primary implementation plan for prompt-submit replay, apply-once persistence, save-for-future prompts, inbox lifecycle labels, and implementation files.
- `docs/superpowers/specs/2026-06-22-session-watcher-design.md` - broader system design for watcher, prompt-submit hooks, stop hooks, and instruction-only replay.
- `docs/plans/2026-06-23-001-feat-beta-aha-loop-plan.md` - predecessor plan for proposal lifecycle visibility and acknowledgement behavior.
- `docs/plans/2026-06-25-001-fix-repeated-prompt-candidates-plan.md` - upstream prompt-pattern candidate source work that feeds prompt-submit replay matching.
