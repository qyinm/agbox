package store

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"errors"
	"fmt"
	"strings"
	"time"
)

const IngestionHealthVersion = 1

type IngestionHealthState string

const (
	HealthHealthy    IngestionHealthState = "healthy"
	HealthCatchingUp IngestionHealthState = "catching_up"
	HealthDegraded   IngestionHealthState = "degraded"
	HealthStalled    IngestionHealthState = "stalled"
)

type HealthDiagnostic struct {
	Field string `json:"field"`
	Code  string `json:"code"`
}

type HealthViolation struct {
	Code   string `json:"code"`
	Detail string `json:"detail"`
}

type HealthCurrentWork struct {
	SourceID   string    `json:"source_id"`
	Generation int64     `json:"generation"`
	Agent      string    `json:"agent"`
	WorkClass  string    `json:"work_class"`
	StartedAt  time.Time `json:"started_at,omitempty"`
}

type HealthProgress struct {
	SourceID       string    `json:"source_id"`
	Generation     int64     `json:"generation"`
	CommittedBytes int64     `json:"committed_bytes"`
	CommittedAt    time.Time `json:"committed_at,omitempty"`
}

type HealthQuarantine struct {
	SourceID       string `json:"source_id"`
	Generation     int64  `json:"generation"`
	Agent          string `json:"agent"`
	Context        string `json:"context"`
	WorkClass      string `json:"work_class"`
	CommittedBytes int64  `json:"committed_bytes"`
	FailureCode    string `json:"failure_code"`
	Retries        int    `json:"retries"`
	NextAction     string `json:"next_action"`
}

// IngestionHealth is the sole versioned operator projection for durable
// ingestion state. It deliberately excludes source paths, projects and
// transcript-derived content.
type IngestionHealth struct {
	Version              int                  `json:"version"`
	State                IngestionHealthState `json:"state"`
	LiveQueueDepth       int                  `json:"live_queue_depth"`
	CatchupQueueDepth    int                  `json:"catchup_queue_depth"`
	OldestLiveLagMS      int64                `json:"oldest_live_lag_ms"`
	Current              *HealthCurrentWork   `json:"current,omitempty"`
	LastProgress         *HealthProgress      `json:"last_progress,omitempty"`
	LastSuccessfulIngest time.Time            `json:"last_successful_ingest,omitempty"`
	HistoryWindowDays    int                  `json:"history_window_days"`
	Consumer             ConsumerState        `json:"consumer"`
	Quarantines          []HealthQuarantine   `json:"quarantines"`
	Violations           []HealthViolation    `json:"violations"`
	Unavailable          []HealthDiagnostic   `json:"unavailable"`
}

func (h IngestionHealth) PlainLines() []string {
	lines := []string{
		"ingestion: " + string(h.State),
		fmt.Sprintf("queue live: %d", h.LiveQueueDepth),
		fmt.Sprintf("oldest live lag: %dms", h.OldestLiveLagMS),
		fmt.Sprintf("queue catch-up: %d", h.CatchupQueueDepth),
	}
	if h.Current == nil {
		lines = append(lines, "current ingest: none")
	} else {
		lines = append(lines, fmt.Sprintf("current ingest: %s generation=%d agent=%s class=%s", h.Current.SourceID, h.Current.Generation, h.Current.Agent, h.Current.WorkClass))
	}
	if h.LastProgress == nil {
		lines = append(lines, "last committed progress: none")
	} else {
		lines = append(lines, fmt.Sprintf("last committed progress: %s generation=%d bytes=%d at=%s", h.LastProgress.SourceID, h.LastProgress.Generation, h.LastProgress.CommittedBytes, formatHealthTime(h.LastProgress.CommittedAt)))
	}
	lines = append(lines, "last successful ingest: "+formatHealthTime(h.LastSuccessfulIngest))
	lines = append(lines, fmt.Sprintf("history window: %dd", h.HistoryWindowDays))
	lines = append(lines, fmt.Sprintf("consumer: %s live=%d catch-up=%d quarantined=%d", h.Consumer.Completeness, h.Consumer.LivePending, h.Consumer.CatchupPending, h.Consumer.Quarantined))
	for _, q := range h.Quarantines {
		lines = append(lines, fmt.Sprintf("quarantine: %s generation=%d agent=%s context=%s class=%s committed=%d failure=%s retries=%d", q.SourceID, q.Generation, q.Agent, q.Context, q.WorkClass, q.CommittedBytes, q.FailureCode, q.Retries))
		lines = append(lines, "next: "+q.NextAction)
	}
	for _, violation := range h.Violations {
		lines = append(lines, fmt.Sprintf("ingestion violation: %s (%s)", violation.Code, violation.Detail))
	}
	for _, unavailable := range h.Unavailable {
		lines = append(lines, fmt.Sprintf("ingestion metric %s: unavailable (%s)", unavailable.Field, unavailable.Code))
	}
	return lines
}

// IngestionHealthAt gathers each independent field separately. A damaged or
// temporarily locked metric is recorded in Unavailable without suppressing
// the other diagnostics.
func (s *Store) IngestionHealthAt(now time.Time) IngestionHealth {
	if now.IsZero() {
		now = time.Now()
	}
	h := IngestionHealth{Version: IngestionHealthVersion, State: HealthHealthy, Quarantines: []HealthQuarantine{}, Violations: []HealthViolation{}, Unavailable: []HealthDiagnostic{}}
	metricError := func(field string, err error) {
		if err != nil {
			h.Unavailable = append(h.Unavailable, HealthDiagnostic{Field: field, Code: healthErrorCode(err)})
		}
	}
	tx, err := s.db.BeginTx(context.Background(), &sql.TxOptions{ReadOnly: true})
	if err != nil {
		metricError("snapshot", err)
		h.State = HealthDegraded
		return h
	}
	defer tx.Rollback()

	metricError("live_queue_depth", tx.QueryRow(`SELECT COUNT(*) FROM ingestion_work WHERE work_class=? AND state IN ('pending','running')`, WorkLive).Scan(&h.LiveQueueDepth))
	metricError("catchup_queue_depth", tx.QueryRow(`SELECT COUNT(*) FROM ingestion_work WHERE work_class IN (?,?) AND state IN ('pending','running')`, WorkActiveCatchup, WorkArchive).Scan(&h.CatchupQueueDepth))

	var oldest sql.NullString
	if err := tx.QueryRow(`SELECT MIN(updated_at) FROM ingestion_work WHERE work_class=? AND state IN ('pending','running')`, WorkLive).Scan(&oldest); err != nil {
		metricError("oldest_live_lag_ms", err)
	} else if oldest.Valid {
		if t := parseTime(oldest.String); !t.IsZero() {
			h.OldestLiveLagMS = max(0, now.Sub(t).Milliseconds())
		}
	}

	var current HealthCurrentWork
	var currentSource, started string
	var currentClass int
	err = tx.QueryRow(`SELECT w.source_id, w.generation, s.agent, w.work_class, w.updated_at
		FROM ingestion_work w JOIN ingestion_sources s USING(source_id,generation)
		WHERE w.state='running' ORDER BY w.updated_at ASC LIMIT 1`).Scan(&currentSource, &current.Generation, &current.Agent, &currentClass, &started)
	if err == nil {
		current.SourceID = operatorSourceID(currentSource)
		current.Agent = safeAgent(current.Agent)
		current.WorkClass = workClassName(WorkClass(currentClass))
		current.StartedAt = parseTime(started)
		h.Current = &current
	} else if !errors.Is(err, sql.ErrNoRows) {
		metricError("current", err)
	}

	var progress HealthProgress
	var progressSource, progressAt string
	err = tx.QueryRow(`SELECT source_id, generation, committed_offset, updated_at FROM ingestion_checkpoints
		WHERE committed_offset > 0 OR visibility_watermark > 0 ORDER BY updated_at DESC LIMIT 1`).Scan(&progressSource, &progress.Generation, &progress.CommittedBytes, &progressAt)
	if err == nil {
		progress.SourceID = operatorSourceID(progressSource)
		progress.CommittedAt = parseTime(progressAt)
		h.LastProgress = &progress
	} else if !errors.Is(err, sql.ErrNoRows) {
		metricError("last_progress", err)
	}

	var successful string
	if err := tx.QueryRow(`SELECT committed_at FROM consumer_visibility WHERE singleton=1`).Scan(&successful); err != nil {
		metricError("last_successful_ingest", err)
	} else {
		h.LastSuccessfulIngest = parseTime(successful)
	}
	var windowSeconds int64
	if err := tx.QueryRow(`SELECT history_window_seconds FROM ingestion_policy WHERE singleton=1`).Scan(&windowSeconds); err != nil {
		metricError("history_window_days", err)
	} else {
		h.HistoryWindowDays = int((time.Duration(windowSeconds) * time.Second) / (24 * time.Hour))
	}
	var consumer ConsumerState
	if err := tx.QueryRow(`SELECT
		COALESCE(SUM(CASE WHEN state IN ('pending','running','waiting_append') THEN 1 ELSE 0 END), 0),
		COALESCE(SUM(CASE WHEN work_class=? AND state IN ('pending','running','waiting_append') THEN 1 ELSE 0 END), 0),
		COALESCE(SUM(CASE WHEN work_class IN (?,?) AND state IN ('pending','running','waiting_append') THEN 1 ELSE 0 END), 0),
		COALESCE(SUM(CASE WHEN state='quarantined' THEN 1 ELSE 0 END), 0) FROM ingestion_work`,
		WorkLive, WorkActiveCatchup, WorkArchive).Scan(&consumer.Pending, &consumer.LivePending, &consumer.CatchupPending, &consumer.Quarantined); err != nil {
		metricError("consumer", err)
	} else {
		switch {
		case consumer.Quarantined > 0:
			consumer.Completeness = ConsumerQuarantined
		case consumer.Pending > 0:
			consumer.Completeness = ConsumerIncomplete
		default:
			consumer.Completeness = ConsumerComplete
		}
		h.Consumer = consumer
	}

	rows, err := tx.Query(`SELECT w.source_id, w.generation, s.agent, w.work_class, c.committed_offset, w.failure_class, w.retry_count
		FROM ingestion_work w JOIN ingestion_sources s USING(source_id,generation)
		JOIN ingestion_checkpoints c USING(source_id,generation)
		WHERE w.state='quarantined' ORDER BY s.agent,w.source_id`)
	if err != nil {
		metricError("quarantines", err)
	} else {
		for rows.Next() {
			var q HealthQuarantine
			var rawID, failure string
			var class int
			if err := rows.Scan(&rawID, &q.Generation, &q.Agent, &class, &q.CommittedBytes, &failure, &q.Retries); err != nil {
				metricError("quarantines", err)
				break
			}
			q.SourceID = operatorSourceID(rawID)
			q.Context = "local " + safeAgent(q.Agent) + " session source"
			q.Agent = safeAgent(q.Agent)
			q.WorkClass = workClassName(WorkClass(class))
			q.FailureCode = safeFailureCode(failure)
			q.NextAction = fmt.Sprintf("agbox sources resume %s --generation %d", q.SourceID, q.Generation)
			h.Quarantines = append(h.Quarantines, q)
		}
		if err := rows.Close(); err != nil {
			metricError("quarantines", err)
		}
	}

	if h.LiveQueueDepth > 0 && h.OldestLiveLagMS > 2000 {
		h.Violations = append(h.Violations, HealthViolation{Code: "live_latency_slo", Detail: "oldest live work exceeds 2s"})
	}
	if h.LiveQueueDepth > 0 && h.Current != nil && h.Current.WorkClass != "live" {
		h.Violations = append(h.Violations, HealthViolation{Code: "queue_starvation", Detail: "catch-up work is running while live work waits"})
	}
	leaseAlive := false
	var expires string
	if err := tx.QueryRow(`SELECT expires_at FROM scheduler_lease WHERE singleton=1`).Scan(&expires); err == nil {
		leaseAlive = parseTime(expires).After(now)
	} else if !errors.Is(err, sql.ErrNoRows) {
		metricError("scheduler_lease", err)
	}
	switch {
	case h.LiveQueueDepth > 0 && h.OldestLiveLagMS > 30_000 && !leaseAlive:
		h.State = HealthStalled
		h.Violations = append(h.Violations, HealthViolation{Code: "scheduler_stalled", Detail: "live work has no active scheduler owner"})
	case len(h.Quarantines) > 0 || len(h.Violations) > 0:
		h.State = HealthDegraded
	case h.CatchupQueueDepth > 0:
		h.State = HealthCatchingUp
	default:
		h.State = HealthHealthy
	}
	return h
}

func (s *Store) IngestionHealth() IngestionHealth { return s.IngestionHealthAt(time.Now()) }

func (s *Store) ResumeSourceByOpaqueID(opaqueID string, expectedGeneration int64, now time.Time) error {
	rows, err := s.db.Query(`SELECT DISTINCT source_id FROM ingestion_sources`)
	if err != nil {
		return err
	}
	defer rows.Close()
	var match string
	for rows.Next() {
		var sourceID string
		if err := rows.Scan(&sourceID); err != nil {
			return err
		}
		if operatorSourceID(sourceID) == opaqueID {
			if match != "" {
				return ErrStateConflict
			}
			match = sourceID
		}
	}
	if err := rows.Err(); err != nil {
		return err
	}
	if match == "" {
		return ErrGenerationMismatch
	}
	return s.ResumeSource(match, expectedGeneration, now)
}

func operatorSourceID(sourceID string) string {
	sum := sha256.Sum256([]byte("agbox-source-v1\x00" + sourceID))
	return "srcop_" + hex.EncodeToString(sum[:12])
}

func workClassName(class WorkClass) string {
	switch class {
	case WorkLive:
		return "live"
	case WorkActiveCatchup:
		return "active_catchup"
	case WorkArchive:
		return "archive"
	default:
		return "unknown"
	}
}

func safeAgent(agent string) string {
	switch agent {
	case "claude", "codex", "cursor", "grok":
		return agent
	default:
		return "unknown"
	}
}

func safeFailureCode(code string) string {
	switch code {
	case FailureSignalTooLarge, FailureRecordBudget, FailureMalformedRecord, FailureMissingContext,
		FailureParse, FailureSourceUnavailable, FailureUnsupportedAdapter, FailureNoProgress:
		return code
	default:
		return "ingestion_failure"
	}
}

func healthErrorCode(err error) string {
	text := strings.ToLower(err.Error())
	switch {
	case strings.Contains(text, "locked"), strings.Contains(text, "busy"):
		return "temporarily_unavailable"
	case strings.Contains(text, "no such table"), strings.Contains(text, "no such column"):
		return "schema_unavailable"
	default:
		return "read_failed"
	}
}

func formatHealthTime(t time.Time) string {
	if t.IsZero() {
		return "never"
	}
	return t.UTC().Format(time.RFC3339)
}
