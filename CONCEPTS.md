# Concepts

Shared domain vocabulary for this project - entities, named processes, and status concepts with project-specific meaning. Seeded with core domain vocabulary, then accretes as ce-compound and ce-compound-refresh process learnings; direct edits are fine. Glossary only, not a spec or catch-all.

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
