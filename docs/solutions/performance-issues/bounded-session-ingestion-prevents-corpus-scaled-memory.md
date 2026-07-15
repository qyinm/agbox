---
title: Bounded session ingestion prevents corpus-scaled memory growth
date: 2026-07-16
category: performance-issues
module: session_ingestion
problem_type: performance_issue
component: background_job
symptoms:
  - "The agbox background ingestion process was reported to create approximately 40 GB of memory pressure as local JSONL session history grew."
  - "Large irrelevant JSONL values can amplify memory use even when their content is not needed for correction extraction."
  - "A single filesystem write can cause repeated corpus-wide discovery and overlapping watcher or CLI ingestion work."
  - "Weak continuation and publication boundaries can replay already-observed history after retries or restarts."
root_cause: logic_error
resolution_type: code_fix
severity: critical
related_components:
  - "database"
  - "tooling"
tags:
  - "bounded-ingestion"
  - "jsonl-streaming"
  - "memory"
  - "scheduler-fencing"
  - "checkpointing"
  - "targeted-refresh"
  - "history-window"
  - "sqlite"
---

# Bounded session ingestion prevents corpus-scaled memory growth

## Problem

agbox's session-ingestion path could turn a small filesystem signal into work proportional to the entire local transcript corpus. The incident investigation found 2,117 Codex JSONL files totaling about 11.273 GiB and reported roughly 40 GB of memory pressure on a 32 GB machine. A single 838.4 MiB source reached about 1.870 GB RSS on its first parse and still reached about 1.867 GB RSS with its cursor already at EOF, while watcher logs showed 260 completed all-source passes (docs/plans/2026-07-16-001-fix-bounded-session-ingestion-redesign-plan.md:42-54).

The failure was not one isolated unreachable allocation. Whole-record or whole-file materialization, pre-cursor replay, corpus-wide discovery, and overlapping watcher/CLI work multiplied one another. The measured per-pass RSS, repeated all-source passes, and absence of a single ingestion owner provide a mechanism consistent with the reported 40 GB aggregate memory pressure; the investigation did not capture a contemporaneous process-by-process RSS breakdown of that peak. The bounded-ingestion redesign is implemented on the current branch and proposed in PR #7, which was still open and unmerged when this document was written.

## Symptoms

- Memory pressure could greatly exceed physical RAM. One large-file parse consumed gigabytes, and repeated passes over an 11.273 GiB corpus provide a mechanism consistent with the reported 40 GB aggregate pressure (docs/plans/2026-07-16-001-fix-bounded-session-ingestion-redesign-plan.md:42-54).
- A cursor at EOF did not make the old path cheap. The measured EOF probe still took about 8.81 seconds and about 1.867 GB RSS, showing that the stored cursor was not bounding historical I/O (docs/plans/2026-07-16-001-fix-bounded-session-ingestion-redesign-plan.md:48-51).
- One file event could amplify into a full-corpus pass. Startup, polling, and debounced triggers could launch all-source ingestion without a single owner or in-flight guard (docs/plans/2026-07-16-001-fix-bounded-session-ingestion-redesign-plan.md:56-64).
- Fresh live records could wait behind about 9.495 GiB of archived sessions, coupling foreground latency to old history volume (docs/plans/2026-07-16-001-fix-bounded-session-ingestion-redesign-plan.md:44-48).
- Parsing correctness was also at risk because Codex-native response_item and event_msg records had previously been sent through an Anthropic-shaped path; the current handler switches on both native record types (internal/session/jsonl/codex.go:13-44).

## What Didn't Work

### Persisting a cursor without using it as the I/O boundary

The old design stored an offset but read from byte zero and replayed pre-cursor records for context. The EOF measurement proves that a cursor alone did not make resource use incremental (docs/plans/2026-07-16-001-fix-bounded-session-ingestion-redesign-plan.md:48-51). A correct checkpoint must be both the durable publication boundary and the file-read boundary.

### Debouncing triggers without establishing one durable owner

Debounce limits trigger frequency; it does not prevent startup, polling, CLI sync, and watcher callbacks from overlapping. The replacement therefore does not rely on timing. It acquires a SQLite-backed lease, rejects another live owner, and increments a fencing token when ownership changes (internal/store/ingestion.go:488-529).

### Fixing only whole-file reads

Replacing io.ReadAll with a buffered reader would reduce one allocation but leave corpus-wide discovery, duplicate work, pre-cursor replay, unbounded individual JSON values, and non-atomic checkpoint publication intact. The parser combines a 32 KiB reader with byte, record-count, depth, token, and time budgets (internal/session/jsonl/stream.go:17-26, internal/session/jsonl/stream.go:165-182, internal/session/jsonl/stream.go:225-247). Streaming alone was not a sufficient goal; the complete unit of work had to be bounded.

### Treating different agent schemas as interchangeable

Sharing a transport parser while pretending Codex and Anthropic records had the same semantics risked silently missing real records. The current native contract lets each handler declare the JSON paths it needs (internal/session/jsonl/stream.go:37-49, internal/session/jsonl/stream.go:80-85), while the Codex handler understands response_item, event_msg, tool calls, and Codex message roles (internal/session/jsonl/codex.go:18-125).

### Automatically ingesting every historical record

Processing every archived session would preserve maximum volume but let stale, noisy evidence dominate startup cost. The redesign defaults the active history window to 90 days (internal/history/policy.go:5-15) and only treats a pre-existing source as eligible when adapter-specific session-time evidence is trusted and inside that window (internal/session/discovery.go:105-119). Old active sources baseline at EOF, so later appends remain eligible without replaying the old body (internal/session/discovery.go:109-119, internal/scheduler/scheduler.go:206-231).

## Solution

### 1. Make parsing checkpoint-bounded and field-selective

ParseNative opens a discovered source through identity-verifying VerifiedOpen, rejects incompatible parser-state versions, and wraps the file in an io.SectionReader capped at the scheduler's claimed target size (internal/session/native.go:13-46). ProcessStream seeks to the committed offset; when that offset equals EOF, it returns before constructing the buffered reader or reading content (internal/session/jsonl/stream.go:139-168).

The essential boundary is:

    // The target is frozen when work is claimed.
    source := io.NewSectionReader(file, 0, claimedTarget)
    result := ProcessStream(source, committedOffset, parserState, handler, meta)

An append that arrives during a slice therefore raises the durable target for the next slice instead of silently extending the current one. Enqueue coalesces by source generation, retains the highest target offset, and upgrades to the highest-priority work class (internal/store/ingestion.go:269-300).

The streaming parser retains only explicitly requested scalar fields. Unselected strings are validated without being accumulated, and selected strings stop retaining bytes after their bounded limit is exceeded (internal/session/jsonl/stream.go:379-468). The defaults include a 64 MiB per-record cap, a 64 MiB inter-record slice-yield threshold, a 1,024-record cap, depth 64, and a 200,000-token cap. A call remains bounded but may consume less than roughly 128 MiB of source data because a record can start just below the slice-yield threshold; the slice deadline is checked between records and every started record receives its own five-second parse deadline (internal/session/jsonl/stream.go:17-26, internal/session/jsonl/stream.go:169-182, internal/session/jsonl/stream.go:235-277).

Only semantically relevant oversized values fail the source. Codex assistant message text may be ignored after its capture limit, while oversized user messages and tool-call inputs return ErrSignalTooLarge because they affect correction extraction (internal/session/jsonl/codex.go:55-125). The relevant-signal limit is 64 KiB (internal/privacy/privacy.go:11). Incomplete final JSONL records do not advance the safe checkpoint; the parser returns the last complete-record offset and marks the result incomplete (internal/session/jsonl/stream.go:185-215).

### 2. Persist bounded continuation state instead of replaying history

The parser serializes only the state needed to link corrections across slices: turn index, last action identity, whether a predecessor action is required, and the last user-message signature (internal/session/jsonl/stream.go:96-128). Store commits reject parser state larger than 32 KiB (internal/store/ingestion.go:21, internal/store/ingestion.go:532-538).

For an old active source intentionally baselined at EOF, MissingContextState records that history was not replayed. A fresh action establishes new linkage, while a correction that truly needs missing context fails diagnostically (internal/session/jsonl/stream.go:131-136, internal/session/jsonl/codex.go:128-153).

### 3. Turn filesystem events into coalesced durable targets

Reconciliation performs metadata-only discovery and writes at most one work row for each source generation; it does not parse transcript content (internal/scheduler/scheduler.go:97-99, internal/scheduler/scheduler.go:238-246). Repeated events update that row with the minimum work-class number—live, active catch-up, then archive—and maximum observed target offset (internal/store/ingestion.go:31-37, internal/store/ingestion.go:288-300).

The common write path avoids a corpus scan. A plain fsnotify.Write refreshes only the cached source's metadata and calls ReconcileLiveSource; create, rename, remove, periodic polling, and fallback errors retain full reconciliation where lifecycle discovery is necessary (internal/watcher/watcher.go:83-149).

Source identity is based on device/inode metadata rather than path alone (internal/session/discovery.go:229-239). Rename preserves a generation, shrink or truncate increments it, path reuse creates a replacement generation, and missing sources are reported deleted (internal/session/discovery.go:171-202). Before parsing, VerifiedOpen uses no-follow semantics and verifies that the opened regular file still has the discovered identity (internal/session/discovery.go:141-155).

### 4. Enforce a single fenced scheduler and strict priority

Each processing attempt acquires the cross-process scheduler lease, recovers stale work, and claims at most one bounded slice (internal/scheduler/scheduler.go:295-321). Work selection is ordered by work_class and then age, and the claim is guarded by the current fence in the same transaction (internal/store/ingestion.go:357-405). Because WorkLive is numerically first, live appends are selected before active catch-up and archive work (internal/store/ingestion.go:31-37, internal/store/ingestion.go:377-388).

CLI sync no longer opens an independent parser path. It reconciles to durable receipts, starts or cooperates with the same scheduler, and waits for receipt completion or quarantine (internal/pipeline/sync.go:21-77, internal/scheduler/scheduler.go:411-455).

### 5. Publish records, checkpoint, visibility, receipt, and queue state atomically

After parsing, the scheduler computes whether the claimed target was reached and sends normalized entities plus the next checkpoint to one store transaction (internal/scheduler/scheduler.go:351-367). That transaction validates the active source generation, expected checkpoint, work claim, fence, and target boundary (internal/store/ingestion.go:532-590). It then writes normalized entities, advances the checkpoint and visibility watermark, updates queue state, and completes only receipts whose target has been reached before committing (internal/store/ingestion.go:592-628).

If any step fails, rollback preserves the old checkpoint and the scheduler requeues claimed work after a commit error (internal/scheduler/scheduler.go:356-366). This is the core crash-safety invariant: a checkpoint is not merely how far parsing got; it is the first byte whose normalized results have not yet been durably published.

### 6. Isolate failures and bound historical influence

Parser failures are classified into signal-size, record-budget, malformed-record, missing-context, and general parse failures (internal/scheduler/scheduler.go:376-388). Quarantine changes only the affected source generation and its work/receipts while leaving its committed checkpoint unchanged (internal/store/ingestion.go:660-705). Resume requires the opaque source ID and exact latest generation, preventing a command for an old generation from resuming its replacement (internal/store/ingestion.go:708-745).

The 90-day window also governs downstream evidence. Active-correction queries filter by the persisted cutoff (internal/store/history.go:36-91), and aging removes expired candidate links, redacts retained correction/action text, recomputes evidence counts, and deactivates candidates below the evidence threshold in one transaction (internal/store/history.go:118-202).

### 7. Make resource limits executable release contracts

The release gate encodes 50 MiB idle and 200 MiB processing RSS ceilings and creates an 838 MiB logical EOF fixture (cmd/agbox-release-gate/main.go:31-36). Its release profile defines 2,500 sources, a 5 GiB logical corpus, 50 records per second, and a 60-second load window (cmd/agbox-release-gate/main.go:47-55). The EOF worker requires zero content bytes read, and the irrelevant-record worker requires a 32 MiB irrelevant value to parse without a correction (cmd/agbox-release-gate/main.go:309-340).

The load worker drives the producer independently of consumer visibility and passes only if catch-up both progresses and is preempted, all records become visible, p95 is at most two seconds, and p99 is at most five seconds (cmd/agbox-release-gate/main.go:457-535).

In the implementation session's release-profile run—not as a hard-coded repository fixture—the 2,500-source/5 GiB/50 records-per-second/60-second case made 3,000 records visible at 98 ms p95 and 100 ms p99 with a measured peak RSS of 14,305,544 bytes. These values are evidence from that run; the durable contract is the threshold encoded by the gate.

This is a backlog-preemption and live-visibility test, not a 5 GiB parse-throughput benchmark. Most catch-up files are sparse logical fixtures, the gate requires catch-up progress rather than complete backlog ingestion, and live updates enter through targeted reconciliation rather than the fsnotify callback. The 200 MiB processing threshold is an executable acceptance boundary, not a claim that every adversarial record shape will stay near the observed implementation-session peak.

## Why This Works

The redesign changes the variables with which resource use scales.

- Memory is bounded by parser budgets instead of corpus size. The parser keeps a 32 KiB buffered reader, selected bounded fields, bounded continuation state, and a bounded result slice; it does not need a generic object for an entire JSONL record (internal/session/jsonl/stream.go:47-52, internal/session/jsonl/stream.go:165-182, internal/session/jsonl/stream.go:396-468).
- I/O is bounded by uncommitted work. Parsing seeks to the committed offset and returns without content reads at EOF (internal/session/jsonl/stream.go:139-163). A claimed target is frozen with a section reader, so concurrent growth becomes later work (internal/session/native.go:42-46).
- Trigger count no longer multiplies work. Every source generation has one durable work identity whose target only moves upward, so repeated events coalesce (internal/store/ingestion.go:288-300).
- Processes cannot both publish. The lease prevents a second live owner, fencing tokens invalidate expired owners, and every claim and commit rechecks the current fence (internal/store/ingestion.go:488-529, internal/store/ingestion.go:547-590, internal/store/ingestion.go:828-833).
- Crash recovery is replay-safe. Normalized entities use idempotent inserts and share a transaction with checkpoint and visibility publication (internal/store/ingestion.go:449-477, internal/store/ingestion.go:592-628).
- Live waiting time is independent of archive backlog depth because live work has strict priority and priority is reconsidered after each bounded slice. It is not mid-record preemption: latency still includes the currently running slice, handler and commit time, and storage contention (internal/store/ingestion.go:31-37, internal/store/ingestion.go:357-405, internal/scheduler/scheduler.go:295-367).
- Old data cannot dominate forever. Trusted metadata gates initial history, old active files baseline at EOF, and expired corrections leave active evidence (internal/session/discovery.go:105-119, internal/store/history.go:118-202).

The resulting scaling model is explicit. Parser memory is proportional to the reader buffer, current bounded captures, bounded normalized results, and parser state rather than total corpus bytes. Slice I/O is bounded by the inter-record threshold plus at most one already-started bounded record. Queue cardinality is proportional to source generations, and the common write path does approximately constant metadata work for the cached source. Periodic/lifecycle discovery and durable storage still grow with source and entity count; bounded parser memory does not make every subsystem constant-time.

## Prevention

### Preserve these invariants in code review

1. No session adapter may materialize an entire source or generic record. New adapters should implement NativeHandler, declare bounded CapturePaths, and use ParseNative (internal/session/jsonl/stream.go:37-49, internal/session/jsonl/stream.go:80-85, internal/session/native.go:13-46). Searches for io.ReadAll, os.ReadFile, enlarged bufio.Scanner buffers, or whole-line json.Unmarshal in runnable adapters are regression signals.
2. No watcher, hook, or CLI path may call an adapter parser directly. Producers should only reconcile metadata or raise durable targets; parsing belongs to the fenced scheduler (internal/scheduler/scheduler.go:97-99, internal/pipeline/sync.go:21-30).
3. Never advance a checkpoint outside the normalized-write transaction. The transaction must validate source generation, fence, expected offset, and claimed target before updating checkpoint and visibility (internal/store/ingestion.go:532-628).
4. Keep queue cardinality proportional to unique source generations. Enqueue must continue using the source-generation conflict key and maximum target rather than event queues or goroutine-per-event work (internal/store/ingestion.go:288-300).
5. Do not replace trusted session-time eligibility with file modification time. Discovery deliberately requires adapter-specific trustworthy session time and baselines ineligible active sources at EOF (internal/session/discovery.go:105-119).

### Keep lifecycle and resource tests mandatory

- Assert EOF parsing performs zero content reads and incomplete records do not move the checkpoint (internal/session/jsonl/stream.go:139-215).
- Assert appending while work is running preserves the current claimed target and leaves the higher target pending (internal/store/ingestion.go:288-300, internal/store/ingestion.go:589-616).
- Assert stale fencing tokens cannot claim or commit, and crash injection cannot publish records without their checkpoint (internal/store/ingestion.go:357-405, internal/store/ingestion.go:532-628).
- Assert rename preserves generation, truncate or path reuse rotates generation, and deletion tombstones old work (internal/session/discovery.go:171-202, internal/store/ingestion.go:249-267).
- Assert oversized irrelevant assistant output is skipped, while oversized correction-relevant user/tool input quarantines only that source (internal/session/jsonl/codex.go:47-125, internal/scheduler/scheduler.go:370-388).
- Run both release-gate profiles after ingestion changes. Smoke gives fast feedback; release exercises 2,500 sources and the full 60-second load window (cmd/agbox-release-gate/main.go:47-55, cmd/agbox-release-gate/main.go:116-159).

### Monitor operational signals instead of raw transcripts

Health should remain derived from queue depth, checkpoint progress, lease validity, quarantine class, and the persisted history window rather than source paths or prompt contents. The health projection reports bounded fields and considers a live queue stalled when lag exceeds 30 seconds without a valid lease (internal/store/health.go:74-102, internal/store/health.go:241-249). Operational telemetry should also track sampled RSS and Go RuntimeSys, bytes/records and wall time per slice, EOF content bytes read, live visibility p95/p99, targeted versus full reconciliation counts, parser-budget quarantines, lease turnover, and stale-fence rejections. This keeps diagnosis useful without recreating an unbounded or privacy-sensitive transcript cache.

PR #7 should not be described as deployed until it is merged and released. Until then, these results validate the pending branch implementation rather than a production rollout.

## Related Issues

- [PR #7: bound session memory and serialize ingestion work](https://github.com/qyinm/agbox/pull/7)
- [Workspace command routing contract regressions](../logic-errors/workspace-command-routing-contract-regressions.md) documents an earlier discovery-cost amplification at the workspace snapshot boundary.
- [Recorded workflow replay lifecycle guardrails](../workflow-issues/recorded-workflow-replay-lifecycle-guardrails.md) documents the downstream replay state machine that consumes durable ingestion output.
