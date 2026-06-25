---
date: 2026-06-25
topic: auto-workflow-record-replay
---

## Summary

agbox becomes an **automatic workflow record and replay layer** for local coding agents. It records workflows passively from local agent sessions, groups repeated work into **Recorded Workflows**, and offers a high-confidence replay suggestion inside the agent before work starts. Replay does **not** re-run shell commands or file edits. v1 replay injects workflow instructions/context, but the UX presents it as a reusable replay plan the user can apply once, validate, and later promote to automatic application.

The product aha is: "agbox noticed this is a workflow I repeat, showed me the plan it learned, and let my agent follow it this time without me re-explaining it."

---

## Problem Frame

The current product can ingest local sessions, detect repeated corrections or prompts, show candidates, and compile approved candidates into skills. That is technically useful, but the user-facing frame still feels like a skill/candidate manager. It does not yet fully express the stronger product promise: **record the user's workflow automatically, then replay the right workflow at the moment it is needed.**

Manual record buttons are not the desired product shape. The differentiator is ambient capture: the user keeps working normally, agbox learns recurring workflows from local sessions, and the next similar request receives a timely replay suggestion.

The main UX risk is trust. If "replay" sounds like automatic command execution, users will worry about unsafe repeated actions. If replay is only a passive inbox card, the aha is weak. The MVP therefore uses a hybrid: inbox for review and control, in-agent suggestion for the aha moment, and instruction-only replay for safety.

---

## Requirements

### Product semantics

- R1. agbox records workflows automatically from local coding-agent sessions. There is no required "start recording" action in the normal path.
- R2. A repeated workflow is presented to users as a **Recorded Workflow**, not as a "candidate" or implementation artifact.
- R3. A Recorded Workflow contains: name, when it applies, replay plan, output contract, evidence, confidence, scope/safety, and lifecycle state.
- R4. A replay plan describes the work pattern the agent should follow, such as files to inspect, checks to run, reasoning order, and response shape. It is not a transcript replay.
- R5. The product language distinguishes between "recorded", "apply once", and "auto-apply" so users understand what is temporary versus persistent.

### Hybrid UX

- R6. The management surface is `agbox inbox`. It lists Recorded Workflow cards and lets users review, edit, snooze, reject, or approve workflows.
- R7. The primary aha happens inside the agent. When the user starts a similar request, agbox can show a concise before-work-start suggestion:
  - Recorded workflow found: `<name>`
  - Apply replay plan this time?
  - Actions: apply once, no, later/view details.
- R8. In-agent suggestions appear only for high-confidence matches that are not rejected, snoozed, or outside scope.
- R9. The suggestion must be short enough not to feel like an interruption. Detailed evidence belongs in inbox/details, not the agent prompt surface.
- R10. The user can always decline or defer a replay suggestion without mutating the workflow into an approved skill.

### Replay behavior

- R11. MVP replay is **instruction replay** presented as **plan replay**. Internally, agbox injects workflow instructions/context into the agent; externally, users see a plan being applied.
- R12. MVP replay must not automatically re-run shell commands, file edits, publishes, git writes, network calls, or tool actions from prior sessions.
- R13. `Apply once` applies the workflow only to the current session/request.
- R14. After an applied workflow appears successful, agbox may ask whether to auto-apply it in the future.
- R15. Auto-apply or skill creation requires explicit user confirmation. Implicit success signals may improve ranking and confidence but cannot make a workflow persistent by themselves.
- R16. The replay payload should include a safety note or badge when useful, e.g. "Instructions only. No commands are re-run automatically."

### Workflow card

- R17. The front of a Recorded Workflow card shows:
  - name
  - when it applies
  - replay plan
  - evidence summary
- R18. Scope/safety appears as a small badge or details section, not as dominant copy on every card.
- R19. Evidence summary should answer why agbox believes this workflow is real, e.g. "Seen 4 times across 2 projects" or "Repeated prompt pattern with consistent follow-up behavior."
- R20. Details view can show source sessions, matched prompts, corrections, and derived steps, while preserving local-first privacy and avoiding unnecessary raw transcript exposure.

### Lifecycle

- R21. Recorded Workflow lifecycle states are:
  - `recorded`: detected but not actively promoted
  - `suggested`: shown inside the agent for a matching request
  - `applied_once`: user applied it to one session/request
  - `validated`: success signals exist after one or more applies
  - `auto_applied`: user explicitly approved future automatic application
  - `rejected` / `snoozed`: user suppressed it
- R22. Existing internal candidate states may remain implementation details, but user-facing copy should map them to the Recorded Workflow lifecycle.
- R23. State changes that affect future behavior must be reversible from inbox.

### Commands and surfaces

- R24. `agbox inbox` is the primary user-facing command for reviewing recorded workflows.
- R25. `agbox beta` may remain as an onboarding alias or transitional surface, but the durable product concept should be inbox/workflows rather than beta candidates.
- R26. `agbox replay` should not be the primary MVP command name because it may imply action replay. A manual replay command can be revisited after the interaction model is trusted.
- R27. `agbox review` can continue to exist if it serves deeper evidence review, but inbox should be the entry point for non-technical workflow management.

---

## Key Decisions

- K1. **Workflow replay over prompt replay.** Replaying only a repeated prompt is too close to the current candidate system. The value comes from capturing trigger, steps, and output contract.
- K2. **Instruction replay over action replay.** Re-running actions is powerful but too risky for MVP. Instruction replay fits agbox's current skill architecture and preserves user trust.
- K3. **Plan-replay UX over raw skill UX.** Users should see a reusable workflow plan, even if the implementation is an injected instruction.
- K4. **Hybrid surface.** Inbox gives control; in-agent suggestion creates the aha moment.
- K5. **Before-work-start suggestions.** High-confidence workflows should be offered before the agent begins so the whole run benefits from the replay plan.
- K6. **Apply once before auto-apply.** One-time application lowers the cost of trying a workflow. Persistent behavior requires a later explicit confirmation.
- K7. **Hybrid validation.** agbox can collect implicit success signals, but auto-apply/skill promotion is always user-approved.
- K8. **`agbox inbox` as the primary command.** It is less risky and more natural than foregrounding `replay` as a command.

---

## Example Flow

1. The user repeatedly asks: "현재 프로젝트 분석해줘".
2. agbox passively records local sessions and notices a recurring workflow:
   - inspect README/docs
   - inspect recent git status/history
   - summarize current state
   - identify risks and next actions
3. `agbox inbox` shows a Recorded Workflow:
   - Name: `Current Project Analysis`
   - When it applies: "When the user asks to analyze the current project or asks what changed."
   - Replay plan: check docs, inspect git state, summarize state, recommend next steps.
   - Evidence: "Seen 3 times in this project."
4. The next time the user asks a similar request, the agent receives:
   - "Recorded workflow found: Current Project Analysis. Apply replay plan this time?"
5. The user chooses `apply once`.
6. agbox injects workflow instructions into the agent context. No prior shell command or file edit is automatically re-run.
7. After the work finishes, agbox may ask:
   - "This replay plan seemed to work. Auto-apply it for future project-analysis requests?"
8. Only if the user confirms does the workflow become auto-applied/a skill.

---

## Success Criteria

- S1. A user can understand from one card what workflow was recorded, when it applies, and what the agent will do differently next time.
- S2. A user who receives an in-agent suggestion can safely try it without worrying that agbox will re-run old commands or mutate files automatically.
- S3. The first high-confidence suggestion feels timely: it appears before the agent starts work and saves the user from re-explaining a known workflow.
- S4. A user can reject, snooze, or edit a workflow from inbox and trust that future suggestions respect that choice.
- S5. Auto-apply never happens without explicit approval.
- S6. Existing local-first privacy posture remains intact: raw sessions stay local, and replay is derived from local evidence.

---

## Scope Boundaries

### In scope for MVP

- User-facing Recorded Workflow terminology
- `agbox inbox` as the primary review/manage command
- High-confidence before-work-start in-agent replay suggestion
- `apply once` behavior via instruction/context injection
- Post-apply validation prompt before auto-apply
- Workflow cards with name, trigger, replay plan, evidence, and scope/safety
- Mapping existing candidates/skills to the Recorded Workflow lifecycle

### Deferred

- Full action replay
- Re-running previous shell commands or tool calls
- Cloud sync or team workflow memory
- Visual workflow builder
- Cross-machine workflow sharing
- LLM-generated workflow editing beyond deterministic or locally-derived MVP heuristics
- Manual `agbox replay <id>` command as a primary workflow
- Rich UI beyond CLI/TUI/in-agent text surfaces

### Outside this product's identity

- Hidden automation that mutates files or executes commands without user approval
- Uploading sessions, prompts, repository paths, or workflow content for remote processing
- Treating every repeated prompt as replay-worthy without evidence of a reusable workflow

---

## Dependencies and Assumptions

- A1. Existing session ingestion, watcher, scan, proposal, evidence, and skill compilation systems remain the implementation foundation.
- A2. Existing internal `candidate` records can back Recorded Workflows, but the UX should not expose "candidate" as the main concept.
- A3. High-confidence matching can begin with deterministic rules and existing semantic keys, then improve over time.
- A4. Agent integration can continue using managed hooks/proposal injection where available.
- A5. Some workflows will be prompt-pattern based and others correction-based; both can become Recorded Workflows if they produce a useful replay plan.

---

## Open Questions

- O1. What minimum evidence is required before a workflow can trigger an in-agent suggestion?
- O2. How much of the replay plan should be user-editable in v1?
- O3. What exact success signals count toward `validated` after `apply once`?
- O4. Should `auto_applied` always compile to a SKILL.md file, or can there be a lighter local rule first?
- O5. Should `agbox beta` become an alias for `agbox inbox`, or remain a separate onboarding/report command during the transition?

---

## Related Implementation Learning

- `docs/solutions/workflow-issues/recorded-workflow-replay-lifecycle-guardrails.md`: post-review guardrails for keeping apply-once and save-for-future states separate, scoping save prompts to same-project replay applications, guarding lifecycle writes, keeping prompt-submit hooks fast, and sanitizing workflow card output.
