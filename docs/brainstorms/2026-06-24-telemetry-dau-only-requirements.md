---
date: 2026-06-24
topic: telemetry-dau-only
---

## Summary

agbox adds **opt-in, anonymous usage telemetry** so maintainers can see **how many opted-in users exist**, **how many are active (DAU/WAU/MAU)**, and **usage streaks** (consecutive active days) without sending prompts, sessions, paths, or machine fingerprints. v1 sends two events per opted-in installation: `install_completed` once and `agbox_daily_active` at most once per UTC day (with `streak_days` on each daily event). Each opt-in gets a **random anonymous ID** for unique counting. Aha-funnel milestones, per-command tracking, error reporting, and north-star aggregates are deferred.

---

## Problem Frame

agbox is local-first and ships with no network telemetry today. The beta plan defers a feedback collector; `agbox beta` produces copyable local output instead. Founders cannot see DAU, WAU, or MAU without voluntary user reports, npm download proxies, or GitHub traffic.

The product README promises that nothing is uploaded. Users who chose agbox for privacy will react strongly to broad telemetry. A multi-agent debate and follow-up dialogue converged on **DAU-only** as the smallest signal that preserves trust while answering “is anyone actually using this?”

---

## Requirements

**Telemetry scope**

- R1. Telemetry is **disabled by default**. No network call related to telemetry occurs until the user explicitly opts in.
- R2. Opt-in is explicit: `agbox telemetry on` lists what will be sent, how to opt out, and requires terminal confirmation before the first transmission.
- R3. Opt-out is immediate: `agbox telemetry off` or `AGBOX_TELEMETRY=0` makes all telemetry paths no-op without affecting other CLI behavior.
- R4. v1 emits only **`install_completed`** (once per opted-in installation) and **`agbox_daily_active`** (at most once per UTC calendar day per opted-in installation).
- R5. Event payloads are limited to: agbox CLI version, OS family, architecture, and an **anonymous installation ID** — a random UUID generated at opt-in, stored locally, and used as the analytics `distinct_id`. agbox does **not** derive this ID from hostname, hardware serial, username, email, or any machine fingerprint. Raw hostname, username, email, repository name, file paths, prompts, session content, skill text, and correction excerpts are never sent.
- R6. Telemetry is best-effort. Failures must not block, delay, or error out CLI commands, watcher startup, or hook execution. All telemetry requests use HTTPS with certificate validation.
- R7. When telemetry is enabled, events are delivered to a maintainer-facing SaaS analytics dashboard (PostHog or equivalent) for: **total opted-in user count** (`install_completed` unique count), **anonymous unique DAU/WAU/MAU**, and **usage streak distribution** (`streak_days` on `agbox_daily_active`). PostHog person profiles are disabled (`$process_person_profile: false`); the random ID enables distinct counting only, not user identity.
- R12. Each `agbox_daily_active` event includes **`streak_days`**: the count of consecutive UTC calendar days this opted-in installation has been active, including today. Streak resets to 1 after a missed UTC day. `streak_days` is computed locally from `last_active_day_utc`; no activity history beyond yesterday is transmitted.

**Trust and disclosure**

- R8. README privacy copy is updated so “Nothing is uploaded” applies to **sessions and prompts**. Anonymous install statistics are described separately and only as opt-in.
- R9. `agbox doctor` or `agbox telemetry status` shows whether telemetry is on or off and where to disable it.
- R10. Install and upgrade flows do not silently enable telemetry. Postinstall must not send telemetry events unless the user has already opted in.

**Product learning (v1 limits)**

- R11. v1 does not emit aha-funnel events (`init`, `beta`, `review`, `approve`, `export`), per-command counts, crash reports, or north-star correction-reduction aggregates.

---

## Key Decisions

- K1. **Opt-in over opt-out.** Default-off telemetry aligns with agbox’s local-first brand and reduces uninstall, fork, and public-trust risk compared with LazyCodex-style default-on DAU.
- K2. **DAU-only v1 over aha funnel.** Funnel milestones answer “where users drop off” but read as workflow surveillance; DAU answers “is the product alive?” with minimal privacy surface.
- K3. **Narrow README promise.** Sessions and prompts stay local; usage statistics are a separate, opt-in category rather than redefining “upload” after the fact.
- K4. **Anonymous unique DAU, not machine fingerprinting.** Unlike LazyCodex/OmO (hostname-derived hash), agbox uses a random UUID at opt-in. The ID counts distinct opted-in installations for DAU but cannot identify a person or reverse to a machine.
- K5. **LazyCodex/OmO as partial pattern reference.** Adopt daily dedup, best-effort PostHog transport, and explicit env opt-out — without hostname-derived IDs, default-on posture, or Codex session hooks as v1 triggers.
- K6. **Founder dashboard via SaaS.** Self-hosted telemetry backend is out of v1 scope; npm/GitHub metrics remain supplementary, not primary.
- K7. **Streak as engagement signal.** Streak answers “are users coming back?” without funnel or command tracking. Local computation keeps payload minimal (one integer per day).

---

## Success Criteria

- S1. A maintainer can view within one week of beta opt-ins: **total opted-in user count**, **anonymous unique DAU/WAU/MAU**, and **streak distribution** (e.g. how many users have 3+, 7+, 30+ day streaks).
- S2. A privacy-conscious user who has not run `agbox telemetry on` can verify (via docs, `doctor`, or network observation) that agbox sends no telemetry.
- S3. README and install-time messaging do not contradict actual behavior for opt-in DAU-only telemetry.
- S4. No GitHub issue template is required solely because v1 telemetry scope exceeded DAU-only opt-in (subjective; revisit after ship).

---

## Scope Boundaries

### In scope (v1)

- Opt-in / opt-out UX for telemetry
- Two events: `install_completed`, `agbox_daily_active` (with `streak_days`)
- UTC-day local deduplication and local streak computation
- Dashboard metrics: total opted-in users, DAU/WAU/MAU, streak distribution
- Privacy README update and status surfacing in doctor
- Maintainer SaaS dashboard wiring

### Deferred for later

- Aha funnel milestones (`init` → `beta` → `review` → `approve` → `export`)
- Per-command event tracking and command distribution
- Anonymous error or crash reporting
- North-star aggregate snapshots (correction reduction across opt-in cohorts)
- Default opt-out telemetry (revisit only if opt-in cohort size proves insufficient)
- Self-hosted telemetry endpoint
- Official `-notelemetry` build artifact (recommended from debate; not pinned as v1 requirement)

### Outside this product's identity

- Uploading prompts, session transcripts, repository paths, or skill content for analytics
- Silent telemetry on by default without explicit user consent
- Cloud team memory, cross-user sync, or shared workflow brain (unchanged from existing product boundaries)

---

## Dependencies and Assumptions

- A1. Maintainer operates or provisions a PostHog (or equivalent) project and API key for ingestion.
- A2. Opt-in participation rate may be low; unique DAU counts represent **consented opted-in installations**, not total npm installs. The anonymous ID is not linkable to a person without local filesystem access.
- A3. agbox currently has no network client code; telemetry introduces a new failure mode class (offline, proxy, DNS) handled best-effort per R6.
- A4. Existing local signals (`agbox beta`, `agbox impact`, SQLite stats) remain the primary per-user feedback path; remote DAU supplements cohort-level survival metrics only.

---

## Resolved Questions

- O1. `install_completed` fires **once per local opt-in** (first confirmation), not on every `init` or postinstall.
- O2. Opt-in is offered **only** via `agbox telemetry on`; init and postinstall do not prompt or send.
- O3. Official `-notelemetry` build is **fast-follow**, not a v1 ship gate.