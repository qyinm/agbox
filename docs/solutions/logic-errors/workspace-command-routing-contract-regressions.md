---
title: Workspace Command Routing Contract Regressions
date: 2026-06-26
category: logic-errors
module: cli_tui_workspace_routing
problem_type: logic_error
component: tooling
symptoms:
  - Embedded Review export target numeric keys were intercepted by workspace navigation
  - "`review --plain` and automation command flags could route before command-specific validation"
  - Interactive status skipped accepted skill reconciliation and could hide remaining health data after one metric error
  - Evidence detail rendered raw terminal-control sequences from evidence summaries
  - Workspace source discovery reran on ordinary navigation instead of only initialization, Sources navigation, or explicit refresh
root_cause: logic_error
resolution_type: code_fix
severity: medium
related_components:
  - assistant
  - testing_framework
tags:
  - charm
  - bubble-tea
  - workspace-routing
  - cli-contracts
  - plain-mode
  - status-health
  - terminal-sanitization
  - source-discovery
---

# Workspace Command Routing Contract Regressions

## Problem

The workspace routing feature introduced a bundle of public CLI and TUI contract regressions in agbox's Charm/Bubble Tea command workspace. This was not a single syntax error; it was several boundary mistakes where workspace-level dispatch started taking precedence over command-specific parsing, review key handling, status side effects, evidence rendering, and source discovery costs.

The fixed behavior was verified with `go test ./internal/cli ./internal/tui` and `go test ./...` using a writable `GOCACHE`.

## Symptoms

- In the Review workspace, `e` opened the export-target picker, but pressing `1` selected the workspace Overview nav item instead of the Review export target.
- `--plain` was stripped before command dispatch, so automation commands such as `capture --plain` could bypass normal unknown-flag validation.
- `review --plain` did not have a deterministic no-TUI guidance path.
- Interactive `agbox status` skipped the accepted-skill reconciliation side effect already present in plain `agbox status`.
- The Status screen could return early on one metric failure and hide still-useful watcher, hook, and last-sync data.
- Evidence detail rendered raw reason, excerpt, and occurrence strings, including terminal-control payloads such as OSC 52 clipboard escapes and SGR color sequences.
- Workspace source discovery ran during ordinary navigation, not only initial load, Sources view, or explicit refresh.

## What Didn't Work

- Fixing only the embedded Review model was not enough because the key event never reached it. Workspace-level numeric shortcuts intercepted `1` through `6` before active-screen handling could delegate to Review.
- Keeping `stripPlainFlag` as a global preprocessing step was too broad. It made workspace routing influence commands that should be parsed independently.
- Treating status data as all-or-nothing hid independent health signals whenever one store query failed.
- Relying on workflow card construction to sanitize all display text missed raw evidence fields that the Evidence screen rendered directly.
- Refreshing sources inside every snapshot refresh tied routine navigation to filesystem/session discovery work even when the user was not looking at Sources.

## Solution

Keep global workspace keys global only until an active screen has a more specific modal claim. `WorkspaceModel.Update` now asks `reviewCapturesKey` whether Review is in export-target mode before it handles workspace nav keys:

```go
key := msg.String()
if m.reviewCapturesKey(key) {
	m = m.handleActiveKey(key)
	return m, nil
}
```

`reviewCapturesKey` is deliberately narrow: it only captures `1` through `4` and `esc` while the active screen is Review and the embedded review model is in `viewExportTarget`. Normal numeric workspace navigation still works everywhere else.

Scope plain-mode stripping to workspace-routed commands. `runCommand` now calls `stripWorkspacePlainFlag`, which removes `--plain` or `--no-tui` only if the resulting command belongs to the workspace command set:

```go
func stripWorkspacePlainFlag(args []string) ([]string, bool) {
	stripped, plain := stripPlainFlag(args)
	if !plain {
		return args, false
	}
	if workspacePlainCommand(stripped) {
		return stripped, true
	}
	return args, false
}
```

This preserves automation command parsing. A command such as `capture --plain` now keeps `--plain` in argv so the capture flag parser can reject it. `review --plain` gets an explicit parsed path: parse review options first, avoid opening the store or TUI, and return the same terminal guidance as noninteractive `agbox review`.

Preserve status side effects before launching the Status workspace. `maybeRunWorkspace` runs `propose.ReconcileAcceptedSkills` for `WorkspaceStatus`, stores the accepted count in `WorkspaceOptions`, and lets the TUI render that result without owning reconciliation logic.

Render Status as partial diagnostics. The screen now renders watcher and managed hook state first, then prints per-field `FAIL` values for stats or corrections errors while still showing fields that are available.

Sanitize raw evidence text at the Evidence screen boundary. The existing workflow display sanitizer is exposed as `workflow.SanitizeDisplayText`, and `renderEvidence` applies it to raw reason, excerpt, and occurrence summary strings before styling them.

Cache source discovery in the workspace snapshot. `NewWorkspaceModel` performs the initial source load. Later snapshot refreshes preserve the previous `sources` and `sourceSummary`, rediscovering only when the active screen is Sources or when explicit refresh asks for a full reload.

## Why This Works

The fixes restore ownership boundaries.

Workspace navigation owns global shortcuts, but embedded screens can own their modal keys. Review export target selection is a modal interaction, so the workspace delegates the conflicting numeric keys only during that modal state.

`--plain` is a compatibility switch for workspace-routed commands, not a universal agbox flag. Stripping it only after workspace command classification keeps automation commands under their own flag parsers. The explicit `review --plain` branch preserves review option validation while avoiding an interactive TUI path.

Status reconciliation belongs at the route where status is launched. Running it before `WorkspaceStatus` keeps plain and interactive status semantically aligned, while passing the count through `WorkspaceOptions` keeps rendering pure.

Health fields are independent observations. A stats failure should not hide watcher, hook, last sync, or reconciliation information. Rendering per-field failures makes the screen useful even during partial store problems.

Evidence text is session-derived and terminal-facing, so it must be treated as hostile display input. Applying the sanitizer at the raw Evidence render boundary covers text that does not flow through workflow card construction.

Source discovery is useful but not necessary for every snapshot. Caching the latest source summary avoids hidden navigation cost while still refreshing on initial load, Sources entry, and explicit refresh.

## Prevention

- Add regression tests around complete user key sequences, not only isolated model states. `a`, `y`, `e`, `1` catches the Review export path because it exercises the workspace shell and embedded model together.
- Do not preprocess flags globally unless every command shares the flag. Workspace-only compatibility flags should be stripped only after command classification.
- For terminal-only commands, add a deterministic no-TUI path for `--plain` and noninteractive stdio, and parse command options before returning guidance.
- Keep interactive command routes semantically aligned with their plain equivalents. If a plain command has a side effect such as accepted-skill reconciliation, the workspace route needs the same side effect or an explicit reason not to.
- Render status/doctor-style screens as partial diagnostics whenever fields are independently obtainable.
- Treat evidence-derived display strings as terminal-hostile. Tests should include both OSC 52 and SGR payloads.
- Cache discovery work that is not required for every snapshot. Tests should assert initial discovery, no rediscovery on unrelated navigation, rediscovery on Sources, and rediscovery on explicit refresh.

## Related Issues

- [Recorded Workflow Replay Lifecycle Guardrails](../workflow-issues/recorded-workflow-replay-lifecycle-guardrails.md) covers adjacent replay lifecycle and evidence-rendering guardrails. The overlap is moderate: both docs touch workflow-facing CLI behavior, evidence terminal sanitization, and regression test placement, but this document focuses on workspace command routing and embedded TUI contracts.
