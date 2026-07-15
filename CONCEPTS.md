# Concepts

Shared domain vocabulary for this project - entities, named processes, and status concepts with project-specific meaning. Seeded with core domain vocabulary, then accretes as ce-compound and ce-compound-refresh process learnings; direct edits are fine. Glossary only, not a spec or catch-all.

## Workspace Command Surface

### Workspace
The interactive agbox command surface that presents command-specific screens for reviewing status, sources, recorded workflows, evidence, repair guidance, and help.

The Workspace is a TUI alternative to line-oriented command output. It must preserve command contracts: scripts and pipes continue to receive plain output, and command-specific parsing still owns unsupported flags.

### Plain Mode
The line-oriented command path that bypasses the Workspace for commands that also have an interactive screen.

Plain Mode is not a universal flag accepted by every agbox command. It is scoped to workspace-routed commands so automation commands retain their own flag validation and error behavior.

### Accepted Skill Reconciliation
The process of detecting persisted skill files that correspond to previously proposed Recorded Workflows and marking those workflows as accepted in agbox's local state.

Reconciliation is a status-side effect: both plain status and interactive Status workspace views should preserve it so the local workflow state reflects skill files the user already has.

## Recorded Workflow Replay

### Recorded Workflow
A reusable workflow pattern that agbox derives from local agent-session evidence and presents as something the user can review, replay once, or save for future use.

A Recorded Workflow is not a command transcript. Its replay behavior is instruction-only: it tells the agent which pattern to follow, while the agent still decides and executes the current request under normal user control.

### Replay Plan
The instruction-oriented plan attached to a Recorded Workflow that describes how the agent should approach a matching request.

A Replay Plan can include investigation order, files or evidence to inspect, checks to run, and expected response shape. It must not promise to re-run prior shell commands, edits, publishes, or tool calls automatically.

### Replay Application
A durable record that a user applied a Recorded Workflow for one request in a particular project context.

Replay Applications are separate from saved workflows. They prove that apply-once happened, but they do not mean the workflow is approved for future automatic use.

### Replay Lifecycle
The set of user-visible workflow states that separate recorded, replay-ready, applied once, save suggested, saved for future, rejected, and snoozed behavior.

Intermediate lifecycle states are not terminal. A workflow that was applied once or has received a save-for-future prompt can still be replay eligible until the user saves, rejects, or snoozes it.

### Apply Once
The explicit user choice to use a Recorded Workflow's Replay Plan for the current request only.

Apply Once creates a Replay Application and may move the workflow into an intermediate lifecycle state, but it must not create a persistent skill or durable future behavior by itself.

### Save For Future
The explicit user choice to promote a tried Recorded Workflow into future reusable agent behavior.

Save For Future is separate from Apply Once. It should only be prompted after a matching Replay Application exists for the current project context, and durable persistence still requires explicit user approval.

## Session Ingestion

### Ingestion Source Generation
A versioned identity for one physical incarnation of a discovered agent-session source, allowing agbox to distinguish continued appends and renames from truncation or path replacement.

Checkpoints, work, receipts, and quarantine state belong to an exact generation so stale work for an earlier incarnation cannot publish into its replacement.

### Ingestion Checkpoint
The durable publication boundary for a Source Generation: bytes before it have had their normalized results and parser continuation committed, while bytes at or after it remain safe to process or retry.

A checkpoint advances atomically with normalized records, visibility, receipt completion, and queue state; it is not merely the furthest byte a parser happened to read.

### Ingestion Receipt
A durable promise that a particular Source Generation will publish results through a requested target boundary or report that the generation was quarantined.

Receipts let explicit sync callers cooperate with the shared ingestion scheduler instead of opening a second parser path.

### Scheduler Fence
The ownership generation attached to the current ingestion scheduler lease, used to reject claims and commits from an owner whose lease expired and was replaced.

Lease expiry permits a new owner to take over; the fence prevents a delayed former owner from publishing after that takeover.

### Active History Window
The bounded interval of trustworthy prior session evidence that automatic catch-up may ingest and downstream evidence selection may continue to use.

Sources outside the window are not automatically replayed from the beginning, while later appends to an old active source can still become live work.
