# Rust v2 Cross-Agent Work Handoff Design Spec

**Date:** 2026-07-17
**Status:** Approved
**Scope:** Clean-slate Rust runtime for local Claude Code ↔ Codex work capture and handoff

---

## Executive Summary

agbox will replace its Go runtime with a clean-slate Rust v2. The new product is
not a line-by-line port of the current correction and recorded-workflow system.
It is a local-first work intelligence runtime that:

1. captures Claude Code and Codex activity,
2. normalizes agent-specific records into immutable, evidence-backed events,
3. builds a work graph that survives session and agent boundaries, and
4. exposes immediately usable work contracts through CLI, TUI, and MCP.

The first release does not execute agents, assign work, sync to a cloud, capture
general ChatGPT conversations, or record screens and audio. Those capabilities
remain later phases. The initial release must first make local Claude ↔ Codex
handoff reliable, bounded, private, and fast.

The final shipping runtime contains no Go. The npm distribution wrapper and web
landing page may remain JavaScript/TypeScript because they are not part of the
native agbox runtime.

---

## Product Direction

The long-term product vision is:

> Capture how people actually work, turn that activity into work that another
> agent can perform, and eventually provide the same capability as a governed
> B2B service.

The planned capability sequence is:

1. Claude Code and Codex session capture
2. cross-agent work extraction and local handoff
3. organization cloud sync and policy controls
4. direct screen, recording, and audio capture
5. broader multimodal work intelligence

screenpipe is a direct future competitor in the capture layer. agbox must
differentiate on what it produces from capture: evidence-backed, executable work
contracts rather than only searchable personal memory.

---

## Approved Decisions

| Area | Decision |
|---|---|
| Runtime language | Fully Rust; no Go runtime code in the final repository |
| Migration style | Clean-slate Rust v2, not a package-by-package port |
| Compatibility | No compatibility requirement for the old DB or CLI |
| Initial agents | Claude Code and Codex only |
| General ChatGPT | Deferred to a later browser/screen integration |
| Initial topology | Local-only daemon and clients |
| First handoff | Claude ↔ Codex in the same local project |
| Work publication | Work contracts are available immediately |
| Human review | No approval queue before a contract becomes available |
| Execution | agbox does not launch agents or execute extracted work |
| Raw-data boundary | Raw sessions remain local |
| Cloud | Added only after local handoff is complete |
| Multimedia | Screen and audio are later sensor adapters |
| Initial platform | macOS arm64; cross-platform abstractions from day one |
| Automatic history | Trusted sources from the most recent 90 days |

---

## Goals

1. Replace the Go runtime with a Rust-native CLI, daemon, TUI, adapters, store,
   MCP server, and local service.
2. Reliably ingest current and future Claude Code and Codex local session
   formats without treating either format as a permanent external standard.
3. Produce an immutable, agent-neutral activity log with exact provenance.
4. Correlate work across sessions and agents without equating a session with a
   task.
5. Publish a provisional work contract immediately and refine it locally as
   more evidence arrives.
6. Let Claude and Codex query the same contract through a shared MCP interface.
7. Keep memory, CPU, I/O, and queue growth independent of total transcript
   corpus size.
8. Keep raw text, tool output, future screen data, and future audio data on the
   local device or customer-controlled network.
9. Detect schema drift and ingestion loss explicitly instead of silently
   omitting unsupported records.
10. Preserve a path to future B2B sync and screen/audio capture without
    implementing either in the first release.

## Non-Goals for Rust v2

- Running, assigning, or scheduling agent work
- Automatically selecting a target agent
- General ChatGPT web or desktop conversation capture
- Screen, video, or audio capture
- Organization cloud sync
- Team permissions or central administration
- Migrating `~/.agbox/agbox.db`
- Preserving the current Go CLI command surface
- Porting every current correction/candidate/export feature
- Storing complete raw transcripts in SQLite
- Treating model reasoning as transferable work context

---

## System Architecture

```text
Claude Code sessions ─┐
                      ├─> source adapters
Codex rollouts ───────┘
                              │
                              ▼
                    bounded ingestion
                              │
                              ▼
                    SourceObservation
                              │
                              ▼
                     ActivityEventV1
                              │
                              ▼
                       WorkGraph reducer
                              │
                              ▼
                  WorkContract revisions
                              │
                    ┌─────────┼─────────┐
                    ▼         ▼         ▼
                   CLI       TUI       MCP
```

The system ships as one native `agbox` binary with multiple commands. A local
daemon owns ingestion and writes. CLI, TUI, and MCP processes communicate with
the daemon over owner-only local IPC.

### Cargo Workspace Boundaries

The initial workspace should remain small:

```text
agbox-core
  immutable domain types and contracts

agbox-adapters
  Claude and Codex discovery and decoding

agbox-ingest
  watchers, cursors, queues, bounded readers, quarantine

agbox-store
  SQLite schema, transactions, queries, encrypted evidence

agbox-workgraph
  correlation, assertions, extraction, contract revisions

agbox-service
  daemon lifecycle, IPC, MCP-facing application services

agbox-cli
  final binary, CLI, TUI, setup, and doctor
```

`agbox-capture` is intentionally absent from the first release. It becomes the
future platform-specific screen/audio sensor crate. Screen and audio must enter
through the same observation/event boundary rather than bypassing it.

---

## Source Format Research

### Research Snapshot

The format analysis used:

- installed Claude Code `2.1.153`,
- installed Codex CLI `0.144.4`,
- local agbox-related transcripts inspected only for keys and JSON types,
- OpenAI Codex source at commit
  `315195492c80fdade38e917c18f9584efd599304`,
- Claude Code public documentation and Agent SDK message contracts.

No local prompt, assistant text, tool argument, or tool output values were
included in the research output.

### Contract Tiers

Each adapter must distinguish three tiers:

1. **Documented contract** — fields and semantics promised in public docs.
2. **Observed contract** — structures seen in current durable local files.
3. **Compatibility envelope** — unknown fields and variants that must not stop
   ingestion.

Neither agent's local persistence format is assumed to be a stable third-party
API.

### Claude Code

Claude Code hooks document a `transcript_path`, but also state that transcript
writes are asynchronous and can lag the in-memory conversation. The hook stream
is therefore a fast signal, while the transcript is the durable recovery source.

The Agent SDK documents message unions such as `assistant`, `user`, `result`,
`system`, streaming events, and compact boundaries. The interactive local
transcript contains additional internal records not covered by one versioned
public transcript schema.

Observed top-level transcript types in Claude Code 2.1.153:

```text
assistant
user
system
attachment
file-history-snapshot
last-prompt
mode
permission-mode
queue-operation
ai-title
```

Observed message envelope fields include:

```text
type
uuid
parentUuid
sessionId
timestamp
cwd
gitBranch
version
isSidechain
entrypoint
message
```

Important semantics:

- user content may be either a string or a content-block array,
- assistant blocks include text, thinking, redacted thinking, and tool use,
- tool requests carry `id`, `name`, and arbitrary JSON `input`,
- tool results appear as user `tool_result` blocks keyed by `tool_use_id`,
- top-level `toolUseResult` can provide additional structured result data,
- `system` records have subtypes such as `turn_duration`,
  `stop_hook_summary`, and `away_summary`,
- subagents have separate JSONL files and linkage fields such as `agentId`,
  `attributionAgent`, and `sourceToolAssistantUUID`,
- `parentUuid` and sidechains make a session a graph rather than a guaranteed
  linear list,
- attachments and file-history snapshots are evidence, not automatically user
  intent or file-change facts.

### Codex

Codex defines its rollout serialization in Rust:

```text
RolloutLine
  timestamp
  ordinal?
  flattened RolloutItem
```

Current `RolloutItem` variants include:

```text
session_meta
response_item
inter_agent_communication
inter_agent_communication_metadata
compacted
turn_context
world_state
event_msg
```

`response_item` persists model-visible items:

```text
message
agent_message
reasoning
local_shell_call
function_call
function_call_output
custom_tool_call
custom_tool_call_output
tool_search_call
tool_search_output
web_search_call
image_generation_call
compaction
```

`event_msg` represents lifecycle and richer runtime events. Relevant variants
include:

```text
task_started
task_complete
turn_aborted
user_message
agent_message
item_completed
mcp_tool_call_end
patch_apply_end
sub_agent_activity
context_compacted
```

Codex persistence varies by `history_mode`:

- legacy rollouts persist selected legacy terminal events,
- paginated rollouts persist `ItemCompleted` with typed `TurnItem` values,
- some begin/progress/command events are transient and never reach the rollout,
- response items remain the most broadly durable model-visible records.

The adapter must detect history mode. It must prefer typed `ItemCompleted`
results in paginated mode and persisted terminal events in legacy mode while
using response items for durable messages and tool call inputs.

Codex protocol enums are explicitly non-exhaustive. Unknown variants are an
expected compatibility case, not a corrupt session.

### Research Sources

- [Claude Code hooks reference](https://code.claude.com/docs/en/hooks)
- [Claude Agent SDK TypeScript message types](https://platform.claude.com/docs/en/agent-sdk/typescript#message-types)
- [Claude tool-use contract](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls)
- [Codex rollout protocol types](https://github.com/openai/codex/blob/315195492c80fdade38e917c18f9584efd599304/codex-rs/protocol/src/protocol.rs)
- [Codex response item types](https://github.com/openai/codex/blob/315195492c80fdade38e917c18f9584efd599304/codex-rs/protocol/src/models.rs)
- [Codex paginated turn items](https://github.com/openai/codex/blob/315195492c80fdade38e917c18f9584efd599304/codex-rs/protocol/src/items.rs)
- [Codex rollout persistence policy](https://github.com/openai/codex/blob/315195492c80fdade38e917c18f9584efd599304/codex-rs/rollout/src/policy.rs)
- [Codex protocol compatibility notes](https://github.com/openai/codex/blob/main/codex-rs/docs/protocol_v1.md)

---

## Immutable Normalization Contract

The system separates source fidelity from agent-neutral meaning:

```text
SourceObservation
  immutable identity and bounded representation of one source record

ActivityEventV1
  immutable agent-neutral observation of a work fact

WorkAssertion
  evidence-backed interpretation used by the work graph
```

Inferred objectives, decisions, and next actions do not belong in
`ActivityEventV1`. They are assertions derived from factual events.

### ActivityEventV1 Envelope

```text
event_id
semantic_key
schema_version
occurred_at
observed_at

project_id
session_id
turn_id?
actor

correlation_id?
causation_id?

source
payload
privacy
```

`actor` is one of:

```text
human
agent
tool
system
```

### SourceRef

```text
provider
format
native_session_id
native_record_type
native_record_id?
source_generation
byte_offset
ordinal?
record_hash
decoder_version
```

### Stable Event Kinds

```text
session.started
session.context_changed

turn.started
turn.finished

message.created

action.requested
action.finished

artifact.changed
plan.observed

agent.started
agent.finished

context.compacted
diagnostic.observed
```

### ContentRef

Large or sensitive values are not embedded in the event:

```text
hash
byte_length
media_type
local_locator
redacted_excerpt
truncated
```

Thinking, raw reasoning, and encrypted reasoning are excluded from transferable
work context. Their existence may be represented by metadata or a hash, but
their content is never promoted into a work contract.

### Source Mapping

| Common fact | Claude Code | Codex |
|---|---|---|
| Session start | First durable record or SessionStart hook | `session_meta` |
| Context change | mode, permission mode, cwd | `turn_context`, thread settings |
| Turn start | non-tool-result user record and `promptId` | `task_started` |
| Human message | user text excluding meta/tool results | user response item; event fallback |
| Agent message | assistant text | assistant response item; event fallback |
| Action request | assistant `tool_use` | function/custom/local-shell call |
| Action result | user `tool_result`, enriched by `toolUseResult` | call output, ItemCompleted, legacy end |
| Artifact change | structured Write/Edit request and result | FileChange or patch terminal event |
| Plan | structured Task/Todo tools | PlanItem |
| Turn finish | turn duration, Stop hook, error | task complete or turn aborted |
| Compaction | compact boundary | compacted/context-compacted |
| Subagent lifecycle | subagent file and agent linkage | parent/fork and subagent activity |
| Diagnostic | hook summary or assistant error | error, abort, terminal status |

### Deduplication and Reconciliation

- `event_id` prevents re-ingesting the same source position.
- `semantic_key` groups multiple records that describe the same logical fact.
- Native message IDs, `tool_use_id`, `call_id`, and turn IDs take priority.
- Content-hash fallback is used only when no native identity exists.
- Claude hooks accelerate observation; transcripts provide durable recovery.
- Codex response items are primary for messages and tool inputs.
- Codex ItemCompleted is preferred in paginated mode.
- Codex persisted terminal events are preferred in legacy mode.
- Fork, sidechain, and parent relationships are preserved rather than flattened.

### Decode Outcomes

```text
known and valid
  -> one or more ActivityEventV1 values

known but malformed
  -> quarantine the complete record

unknown type
  -> UnknownObservation and schema-drift metric

incomplete final line
  -> do not advance the cursor

oversized content
  -> bounded ContentRef, never unbounded allocation
```

Unknown additive fields do not fail a source. Missing identity or correlation
fields required for a known semantic record do produce a visible degraded
adapter state.

---

## Bounded Ingestion

One filesystem event must never cause a corpus-wide parse.

```text
filesystem or hook signal
        -> keyed bounded queue
        -> incremental source reader
        -> source decoder
        -> normalizer
        -> single SQLite writer
        -> WorkGraph reducer
```

### Scheduling Rules

- Queue keys are logical source generations.
- Repeated events for one source coalesce.
- Worker count and queue capacity are fixed.
- Live source work preempts historical discovery.
- Archive discovery is low priority and independently bounded.
- Directory enumeration yields after a bounded number of entries.
- No unbounded task spawn is allowed.

### History Policy

- Automatically ingest sources with a trustworthy session time within 90 days.
- Baseline older sources at EOF without reading their content.
- A later append to an old active source is live work and is ingested.
- Sources without trustworthy session time are not automatically replayed.

### Source and Cursor State

```text
source_id
provider
root_class
path
file_identity
generation
size
mtime
session_time
cursor_offset
parser_state
schema_fingerprint
status
```

Moves preserve logical source identity when trustworthy. Replacement or
truncation creates a new generation. Previous generation events remain valid.

### Streaming Limits

Initial resource contracts:

```text
read buffer                  64 KiB
inline semantic field        64 KiB
redacted preview              2 KiB
ingestion batch               4 MiB or 1,000 records
queue capacity               fixed and observable
in-flight source jobs        small fixed value
```

The decoder must not materialize an arbitrary JSONL line as a complete string or
generic JSON value. It extracts selected paths while streaming, hashes the raw
record, and records local ranges for larger content.

### Transaction Boundary

One committed chunk atomically writes:

1. source observations,
2. activity events,
3. evidence links,
4. cursor and parser state,
5. schema fingerprints and faults.

The cursor advances only with the transaction. A crash before commit causes a
safe replay; deterministic event IDs prevent duplicates.

SQLite runs in WAL mode. One writer task serializes mutations, while independent
read connections serve CLI, TUI, and MCP.

### Hook Spool

Hooks send bounded payloads through `agbox hook ingest`. If the daemon is
temporarily unavailable, the command writes to an owner-only bounded local
spool. A hook is never the only source of truth, particularly for Codex, where
hook availability can vary by product path.

---

## Local Persistence

Logical storage areas:

```text
sources
source_generations
source_cursors
source_observations
activity_events
event_evidence
content_refs
schema_fingerprints
ingestion_faults

projects
agent_runs
work_items
work_assertions
work_edges
artifacts
work_evidence
work_contract_revisions
extractor_runs
handoff_reads
```

Rust v2 uses `~/.agbox/state.db`. It does not open or migrate the old
`~/.agbox/agbox.db`, and it never deletes that file automatically.

---

## WorkGraph

A session is not a task. `WorkItem` is the agent-independent unit that survives
session changes and handoff.

```text
Project
  WorkItem
    WorkAssertion
    WorkContractRevision
    Evidence
    Artifact
    AgentRun
  WorkEdge
```

Initial work edges:

```text
continues
depends_on
blocked_by
produces
validated_by
supersedes
```

### Work Correlation Signals

Higher-confidence signals take precedence:

1. explicit work, issue, PR, or handoff ID,
2. explicit continuation of an existing work item,
3. project, repository, and branch identity,
4. overlapping artifacts and commands,
5. temporal proximity,
6. semantic goal similarity.

Semantic similarity alone may propose a link but must carry lower confidence.
The evidence remains available so a later revision can split a mistaken merge.

### Two-Stage Extraction

The deterministic reducer runs immediately:

```text
new events
  -> sessions, actions, artifacts, verification facts
  -> provisional WorkContract revision
```

A local semantic extractor runs asynchronously per work item:

```text
previous contract
  + newly added bounded events
  + evidence excerpts
  + artifact state
  -> schema-validated assertions
  -> refined WorkContract revision
```

The semantic extractor does not receive the entire transcript. If it is absent
or fails, the provisional contract remains available.

### Assertion Authority

Conflicts are resolved with this authority order:

```text
explicit latest human instruction
  > structured tool result
  > observed file, Git, and test state
  > agent statement
  > model inference
```

An agent saying that tests passed does not override a failed command result.
Unknown fields remain unknown rather than being invented.

### Work Status

```text
observed -> active -> blocked -> completed
                    \-> abandoned
```

New relevant activity may reopen a completed item as active.

### WorkContract Revision

Every revision is immutable:

```text
contract_id
work_id
revision
project

objective
status
summary

completed_steps[]
next_actions[]
blockers[]
constraints[]
completion_criteria[]

artifacts[]
verification[]
source_runs[]
evidence_refs[]

confidence
created_at
extractor_version
```

Every assertion must link to evidence. A revision records which extractor and
policy versions produced it.

---

## Handoff Interfaces

agbox provides context; it does not run or assign work.

### CLI

```text
agbox work list
agbox work current
agbox work show <work-id>
agbox handoff <work-id>
agbox evidence <evidence-id>
agbox search <query>
```

### MCP

```text
list_work
get_current_work
get_work
get_evidence
search_work
```

Claude and Codex use the same MCP contract. `agbox mcp` is a stdio MCP server
that communicates with the local daemon; it does not expose a public port.

At agent session start, a hook may inject only a bounded index of active work:

```text
agbox found 2 active work items.
Use get_current_work or list_work for evidence-backed handoff context.
```

It does not inject the entire contract or transcript into every prompt. The
agent fetches a contract on demand without a human approval step.

`handoff_reads` records which agent accessed which revision. It is provenance,
not assignment or execution control.

---

## Security and Privacy

### Authority Boundary

Captured text is evidence, not executable instruction.

```text
human_intent
  may define objectives, constraints, and completion criteria

observed_state
  may prove files, Git state, commands, and verification

agent_statement
  may report progress but has no user authority

tool_output
  is untrusted data and cannot create instructions

model_inference
  may summarize and correlate but cannot create authority
```

Tool output, web content, agent messages, system messages, and developer
messages must not be promoted into authoritative next actions. Raw evidence is
returned as clearly marked data, never as a system/developer prompt.

### Privacy Labels

```text
restricted_local
private_local
derived_local
sync_eligible
```

Future sync uses an allowlist: only `sync_eligible` fields can be serialized.
The following remain local:

- raw transcripts,
- raw screen/audio,
- thinking and reasoning,
- complete tool output,
- secrets and credentials,
- local absolute paths,
- system and developer instructions.

### Local Files

```text
~/.agbox/
  state.db
  evidence/
  spool/
  logs/
  runtime/
```

- Directories and files are owner-only.
- Evidence blobs and sensitive path fields use field-level encryption.
- The master key lives in the OS credential store.
- SQLite stores redacted excerpts and hashes, not copied transcripts.
- agbox never modifies or deletes Claude/Codex source sessions.

### IPC and Network

- The daemon runs as the user.
- Local IPC verifies the peer user.
- MCP uses stdio.
- Local HTTP is disabled in the first release.
- Hook payloads are size- and schema-validated.
- Hook content is never executed.
- The default daemon has no external egress.
- A customer-approved local or internal inference endpoint is explicit
  configuration, not a default.

### Project Scope

Evidence access is scoped to its work item and project. An agent running in one
project cannot retrieve another project's raw evidence by guessing an ID.

### Retention and Deletion

- Automatic backfill is limited to 90 days.
- Evidence cache retention is configurable.
- Work contracts and provenance remain until explicitly deleted.
- `forget work` and `forget project` delete agbox-owned data only.
- Source transcript deletion remains the responsibility of the source agent.
- Contract reads, extraction, policy decisions, and deletion emit local audit
  events.

---

## Product UX

### Setup

```text
agbox init
  detect Claude Code and Codex
  initialize state.db
  install managed MCP entries and supplemental hooks
  install and start the daemon
  schedule bounded discovery
  run doctor
```

Agent configuration changes are idempotent and preserve unknown user/plugin
settings.

### Command Groups

```text
agbox init
agbox status
agbox doctor

agbox daemon start|stop|logs
agbox agent list|connect|disconnect
agbox work list|current|show
agbox handoff
agbox evidence
agbox search
agbox tui
agbox mcp
agbox config show|set
agbox forget work|project
```

### TUI

The TUI centers on:

- active, blocked, and completed work,
- contract revisions,
- Claude/Codex agent runs and handoffs,
- evidence and assertion provenance,
- ingestion lag and faults,
- schema drift,
- privacy and retention state.

It is not a human approval queue. A manual correction creates a new
human-authority assertion rather than editing immutable events.

---

## Packaging and Cutover

The npm package remains a thin platform detector and native-binary downloader.
It contains no product business logic.

Release order:

1. macOS arm64,
2. macOS x64 and Linux x64/arm64,
3. Windows x64,
4. platform-specific screen/audio capture.

Filesystem, path, process, service, credential-store, and IPC behavior use
platform abstractions from the first implementation.

### Independent Rewrite

The Go and Rust runtimes are not connected through IPC:

```text
existing Go agbox      independent legacy runtime
new Rust v2            independent state.db and product runtime
```

Cutover occurs only after all Rust release gates pass:

1. switch npm packaging and release automation to Rust,
2. remove Go `cmd/`, `internal/`, `go.mod`, and `go.sum`,
3. verify no Go runtime source remains,
4. retain the old published Go release as a package rollback option.

The new state DB is not readable by a rolled-back Go version.

---

## Verification Strategy

### Adapter Fixtures

Sanitized contract corpora cover:

```text
Claude 2.1.x
Claude subagents and sidechains
Claude malformed and schema-drift records

Codex legacy history
Codex paginated history
Codex subagents
Codex malformed and schema-drift records
```

Required cases include:

- string and array user content,
- parallel tool calls,
- request/result correlation,
- response/event duplication,
- unknown fields and variants,
- incomplete UTF-8 and JSONL,
- reasoning and system-instruction exclusion.

Decoders receive fuzz and property tests. Arbitrary input must not panic,
deadlock, loop forever, or allocate in proportion to an oversized field.

### Recovery Cases

- partial final line,
- truncation and replacement,
- move to archive,
- duplicate filesystem notifications,
- crash before and after commit,
- SQLite busy and disk-full behavior,
- oversized relevant and irrelevant fields,
- one malformed record among valid records,
- old and undated sources.

Every case verifies cursor durability, exact-once events, source isolation,
visible health, and restart recovery.

### Cross-Agent End-to-End Gate

1. Claude begins a task and leaves it active.
2. agbox publishes a work contract.
3. Codex retrieves it in the same project.
4. Codex completes and verifies the work.
5. one WorkItem contains both AgentRuns and a completed revision.

The reverse Codex → Claude flow must also pass.

### Security Cases

Search the DB, FTS index, evidence cache, logs, MCP output, TUI snapshots, and
crash output for fixture secrets. Any plaintext secret in a forbidden sink
fails release. Prompt-injection fixture content must remain evidence and must not
become human-authority instructions.

### Performance Gate

```text
logical corpus             5 GiB
sources                    2,500+
append rate                50 records/s for 60 seconds
visible records            3,000+
ingestion p95              < 100 ms
ingestion p99              < 200 ms
peak daemon RSS            < 256 MiB
838 MiB source at EOF      0 content bytes read
oversized record           bounded memory independent of field size
MCP current-work p95       < 200 ms
```

Local model memory is measured separately. A model-free provisional contract
must pass the latency gate.

Additional gates:

- no idle corpus-wide scans,
- bounded affected sources per filesystem event,
- explicit backpressure instead of queue-driven memory growth,
- no sustained RSS growth in a 24-hour soak,
- identical event counts across repeated crash/restart tests.

### Local Observability

`agbox doctor` and the TUI expose:

```text
queue depth
source lag
bytes and records read
decode latency
commit latency
unknown schema count
quarantined records
contract revision latency
MCP query latency
process RSS
```

---

## Rust v2 Release Boundary

The first release is complete only when it includes:

- Claude Code 2.1.x transcript and subagent capture,
- Codex legacy and paginated rollout capture,
- immutable ActivityEventV1,
- WorkGraph and immediate WorkContract revisions,
- bidirectional Claude/Codex MCP handoff,
- CLI, TUI, doctor, and daemon,
- local-only privacy and encrypted evidence,
- schema-drift reporting,
- bounded ingestion release gates,
- macOS arm64 native packaging,
- zero Go runtime source after cutover.

Cloud sync, automatic execution, general ChatGPT capture, and screen/audio
capture remain explicit later phases.

---

## Implementation Planning Boundary

This document defines product and architecture decisions. Concrete Rust library
selection, task ordering, per-crate APIs, migration commits, and implementation
checkpoints belong in the subsequent implementation plan.

Implementation planning begins only after this spec is reviewed.
