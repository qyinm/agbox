# PostHog telemetry dashboard (maintainer)

**Audience:** agbox founders / maintainers only.  
**Purpose:** Configure PostHog so you can read anonymous usage—total installations, DAU/WAU/MAU, and streaks—without SQL.

> **Important:** Telemetry is **on by default**; users opt out with `agbox telemetry off` or `AGBOX_TELEMETRY=0`. Metrics count installations that have not opted out and have a configured `POSTHOG_API_KEY` (bundled at npm publish or in `~/.agbox/.env`). They do **not** reflect total npm installs when capture is not configured.

---

## Events

| Event | When it fires | Frequency |
|-------|---------------|-----------|
| `agbox_install_completed` | First successful capture after install | Once per installation |
| `agbox_daily_active` | First foreground `Execute` per UTC calendar day (excluding `init`, `telemetry`, `help`) | At most once per UTC day per installation |

`distinct_id` on every capture is the installation’s random `anonymous_id` (UUID). The same value is also sent as the `anonymous_id` event property.

---

## Event properties

| Property | `agbox_install_completed` | `agbox_daily_active` | Notes |
|----------|:-------------------------:|:--------------------:|-------|
| `app` | ✓ | ✓ | Product identifier (`agbox`); filter per app in a shared PostHog project |
| `agbox_version` | ✓ | ✓ | Build version string |
| `os_family` | ✓ | ✓ | e.g. `darwin`, `linux`, `windows` |
| `arch` | ✓ | ✓ | e.g. `amd64`, `arm64` |
| `anonymous_id` | ✓ | ✓ | Random UUID; matches `distinct_id` |
| `streak_days` | — | ✓ | Consecutive UTC active days including today |

Every capture also sets **`$process_person_profile: false`** in properties. PostHog must not build person profiles from these events; counting uses `distinct_id` only.

---

## PostHog project configuration

Clients send events to PostHog’s `/capture/` endpoint. Maintainers (or release CI) must provide credentials so opted-in clients can deliver events.

### Environment variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `POSTHOG_API_KEY` | Yes (for live telemetry) | — | Project API key (`phc_…`). Without it, opted-in telemetry is a no-op (no network calls). |
| `POSTHOG_HOST` | No | `https://us.i.posthog.com` | PostHog ingest host (use EU host if your project is in EU). |

See [`.env.example`](../.env.example) at the repo root for a template.

### Where to set them

1. **Process environment** — export before running agbox (highest precedence).
2. **`~/.agbox/.env`** — copy from `.env.example` for local maintainer testing.

Resolution order: existing process env wins; `~/.agbox/.env` fills in only keys not already set in the environment.

---

## Insights to create

Create a PostHog dashboard (e.g. **agbox — usage**) with the following insights. When multiple apps share one PostHog project, add filter **`app = agbox`** on every insight.

### 1. Total opted-in users (all time)

- **Type:** Trends (or formula)
- **Event:** `agbox_install_completed`
- **Aggregation:** Unique users (`distinct_id`)
- **Date range:** All time

This is the count of installations that have ever completed opt-in, not npm download count.

### 2. DAU / WAU / MAU

- **Type:** Trends
- **Event:** `agbox_daily_active`
- **Aggregation:** Unique users (`distinct_id`)
- **Intervals:** Add separate series or tiles for **Day**, **Week**, and **Month**

Each period counts unique opted-in installations that sent at least one `agbox_daily_active` event in that window.

**Undercount note:** Daily active fires on the first foreground `Execute` per UTC day. Background watcher-only use on a day does not emit an event, so DAU/WAU/MAU may be lower than true usage.

### 3. Streak distribution

- **Type:** Trends with breakdown, or HogQL / histogram
- **Event:** `agbox_daily_active`
- **Breakdown:** Property `streak_days` (numeric)
- **Visualization:** Bar chart or histogram of `streak_days` values

Useful views:

- Full distribution of `streak_days` across daily events.
- Filter `streak_days >= 7` to approximate “power users” on a 7+ day streak.

`streak_days` is computed locally: consecutive UTC calendar days active including today; resets to `1` after a missed UTC day.

### 4. Retention (optional)

- **Type:** Retention
- **Target event:** `agbox_daily_active`
- **Returning event:** `agbox_daily_active`
- **Period:** Day (UTC-aligned with client behavior)

Shows what share of opted-in installations return on subsequent days/weeks.

---

## Privacy constraints (do not change in PostHog)

- **`$process_person_profile: false`** is set on every capture—do not enable person profiling for this project’s agbox pipeline.
- No `$identify`, `$set`, or PII fields are sent.
- Do not join `anonymous_id` to npm, GitHub, or other identity sources.

---

## Quick verification checklist

After configuring the dashboard, confirm:

1. **Opted-in user count** — `agbox_install_completed` unique `distinct_id` matches expectations after a test opt-in.
2. **DAU** — Running agbox once on a UTC day produces one `agbox_daily_active` for that `distinct_id`.
3. **Streaks** — Second consecutive UTC day shows `streak_days: 2` on the daily event.
4. **Cohort scope** — Metrics move only when users run `agbox telemetry on`; they are unrelated to total install volume.