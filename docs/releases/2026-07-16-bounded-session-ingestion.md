# Bounded session ingestion

This release replaces direct whole-file session sync with a single-owner,
SQLite-backed ingestion scheduler. It addresses corpus-proportional memory use,
overlapping sync paths, and live records waiting behind old history.

## Upgrade notice: local agbox history is reset

The first open of a verified legacy agbox database performs a one-time,
generation-gated reset. The legacy database and its WAL/SHM files are deleted
before the new schema is created.

**This reset has no backup, rollback, or recovery path.** Existing candidates,
evidence links, checkpoints, and other agbox-owned results are discarded. Agent
session files are not deleted and remain the local source of truth.

## What changed

- Active roots are watched before recent-history catch-up starts.
- Live appends preempt active-history and archive work.
- Automatic active/archive catch-up uses a 90-day window. Older active files
  baseline at EOF, but new appends to them are processed as live work.
- JSONL parsing seeks to the committed checkpoint and keeps bounded, versioned
  continuation context. A checkpoint already at EOF reads no content bytes.
- One fenced scheduler owner commits normalized records, checkpoint state, and
  consumer visibility atomically. Trigger storms coalesce to one target offset.
- A malformed or oversized relevant record quarantines only its source. Resume
  it explicitly with the opaque ID and expected generation shown by doctor:

  ```bash
  agbox sources resume <opaque-source-id> --generation <N>
  ```

- `status` and `doctor` share privacy-safe `healthy`, `catching_up`, `degraded`,
  and `stalled` ingestion states.
- Cursor source discovery remains available, but Cursor transcript parsing is
  unsupported until a stable native schema and real fixtures are available.

## Privacy and resource contracts

Session files stay local. agbox does not add raw transcript storage, uploads, or
raw-record diagnostics. Health, receipts, errors, and recovery commands expose
opaque source identity and bounded metadata only.

For the supported macOS arm64 build, the release contracts are:

- idle watcher RSS <= 50 MiB;
- processing RSS <= 200 MiB;
- 838 MiB committed-EOF source: zero content reads;
- 32 MiB irrelevant JSON value: bounded processing;
- 2,500 sources / 5 GiB logical corpus at 50 records per second for 60 seconds:
  live visibility p95 <= 2 seconds and p99 <= 5 seconds while catch-up exists.

The generated-fixture harness emits machine-readable JSON and exits non-zero on
failure:

```bash
go run ./cmd/agbox-release-gate --profile smoke
go run ./cmd/agbox-release-gate --profile release
```

CI runs the smoke profile. The full 60-second profile is an explicit manual
workflow gate so normal pull-request feedback remains fast.
