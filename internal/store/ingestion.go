package store

import (
	"database/sql"
	"errors"
	"fmt"
	"strings"
	"time"
)

var (
	ErrGenerationMismatch = errors.New("source generation mismatch")
	ErrStateConflict      = errors.New("ingestion state conflict")
	ErrLeaseHeld          = errors.New("scheduler lease held")
	ErrStaleFence         = errors.New("stale scheduler fence")
)

const MaxParserStateBytes = 32 << 10

type SourceState string

const (
	SourceActive      SourceState = "active"
	SourceTombstoned  SourceState = "tombstoned"
	SourceQuarantined SourceState = "quarantined"
)

type WorkClass int

const (
	WorkLive WorkClass = iota
	WorkActiveCatchup
	WorkArchive
)

type WorkState string

const (
	WorkPending     WorkState = "pending"
	WorkRunning     WorkState = "running"
	WorkComplete    WorkState = "complete"
	WorkQuarantined WorkState = "quarantined"
)

type ReceiptStatus string

const (
	ReceiptAccepted    ReceiptStatus = "accepted"
	ReceiptCompleted   ReceiptStatus = "completed"
	ReceiptQuarantined ReceiptStatus = "quarantined"
)

type SourceGeneration struct {
	SourceID   string
	Generation int64
	Agent      string
	SourceRef  string
	State      SourceState
	CreatedAt  time.Time
	UpdatedAt  time.Time
}

type IngestionCheckpoint struct {
	SourceID            string
	Generation          int64
	CommittedOffset     int64
	ParserStateVersion  int
	ParserState         []byte
	VisibilityWatermark int64
	UpdatedAt           time.Time
}

type IngestionWork struct {
	SourceID     string
	Generation   int64
	Class        WorkClass
	TargetOffset int64
	State        WorkState
	RetryCount   int
	FailureClass string
	ActiveFence  int64
	CreatedAt    time.Time
	UpdatedAt    time.Time
}

type EnqueueWork struct {
	SourceID     string
	Generation   int64
	Class        WorkClass
	TargetOffset int64
	ReceiptID    string
	Now          time.Time
}

type SchedulerLease struct {
	OwnerID      string
	FencingToken int64
	ExpiresAt    time.Time
	HeartbeatAt  time.Time
}

type SliceCommit struct {
	SourceID            string
	Generation          int64
	ExpectedOffset      int64
	NextOffset          int64
	ParserStateVersion  int
	ParserState         []byte
	VisibilityWatermark int64
	ReceiptID           string
	LeaseOwner          string
	FencingToken        int64
	Now                 time.Time
	Complete            bool
}

type QuarantineRequest struct {
	SourceID       string
	Generation     int64
	ExpectedOffset int64
	FailureClass   string
	LeaseOwner     string
	FencingToken   int64
	Now            time.Time
}

type IngestionReceipt struct {
	ReceiptID    string
	SourceID     string
	Generation   int64
	TargetOffset int64
	Status       ReceiptStatus
	FailureClass string
	CreatedAt    time.Time
	CompletedAt  time.Time
}

func (s *Store) UpsertSourceGeneration(source SourceGeneration) error {
	if strings.TrimSpace(source.SourceID) == "" || source.Generation <= 0 || strings.TrimSpace(source.Agent) == "" || strings.TrimSpace(source.SourceRef) == "" {
		return fmt.Errorf("%w: incomplete source identity", ErrGenerationMismatch)
	}
	if source.State == "" {
		source.State = SourceActive
	}
	if source.CreatedAt.IsZero() {
		source.CreatedAt = time.Now()
	}
	if source.UpdatedAt.IsZero() {
		source.UpdatedAt = source.CreatedAt
	}
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	var agent, sourceRef string
	err = tx.QueryRow(`SELECT agent, source_ref FROM ingestion_sources WHERE source_id = ? AND generation = ?`, source.SourceID, source.Generation).Scan(&agent, &sourceRef)
	if err == nil && (agent != source.Agent || sourceRef != source.SourceRef) {
		return fmt.Errorf("%w: immutable source identity changed", ErrGenerationMismatch)
	}
	if err != nil && !errors.Is(err, sql.ErrNoRows) {
		return err
	}
	_, err = tx.Exec(`INSERT INTO ingestion_sources(source_id, generation, agent, source_ref, state, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(source_id, generation) DO UPDATE SET state=excluded.state, updated_at=excluded.updated_at`,
		source.SourceID, source.Generation, source.Agent, source.SourceRef, source.State, formatTime(source.CreatedAt), formatTime(source.UpdatedAt))
	if err != nil {
		return err
	}
	_, err = tx.Exec(`INSERT INTO ingestion_checkpoints(source_id, generation, committed_offset, parser_state_version, parser_state, visibility_watermark, updated_at)
		VALUES (?, ?, 0, 0, X'', 0, ?) ON CONFLICT(source_id, generation) DO NOTHING`, source.SourceID, source.Generation, formatTime(source.UpdatedAt))
	if err != nil {
		return err
	}
	return tx.Commit()
}

func (s *Store) EnqueueIngestionWork(input EnqueueWork) (IngestionWork, error) {
	if input.TargetOffset < 0 || input.Generation <= 0 {
		return IngestionWork{}, fmt.Errorf("%w: invalid work target", ErrGenerationMismatch)
	}
	if input.Now.IsZero() {
		input.Now = time.Now()
	}
	tx, err := s.db.Begin()
	if err != nil {
		return IngestionWork{}, err
	}
	defer tx.Rollback()
	var exists int
	if err := tx.QueryRow(`SELECT COUNT(*) FROM ingestion_sources WHERE source_id = ? AND generation = ?`, input.SourceID, input.Generation).Scan(&exists); err != nil {
		return IngestionWork{}, err
	}
	if exists != 1 {
		return IngestionWork{}, ErrGenerationMismatch
	}
	_, err = tx.Exec(`INSERT INTO ingestion_work(source_id, generation, work_class, target_offset, state, retry_count, failure_class, active_fence, created_at, updated_at)
		VALUES (?, ?, ?, ?, 'pending', 0, '', 0, ?, ?)
		ON CONFLICT(source_id, generation) DO UPDATE SET
			work_class=MIN(ingestion_work.work_class, excluded.work_class),
			target_offset=MAX(ingestion_work.target_offset, excluded.target_offset),
			state=CASE
				WHEN ingestion_work.state = 'quarantined' THEN 'quarantined'
				WHEN excluded.target_offset > COALESCE((SELECT committed_offset FROM ingestion_checkpoints WHERE source_id=excluded.source_id AND generation=excluded.generation), 0) THEN 'pending'
				ELSE ingestion_work.state END,
			updated_at=excluded.updated_at`,
		input.SourceID, input.Generation, input.Class, input.TargetOffset, formatTime(input.Now), formatTime(input.Now))
	if err != nil {
		return IngestionWork{}, err
	}
	if input.ReceiptID != "" {
		_, err = tx.Exec(`INSERT INTO ingestion_receipts(receipt_id, source_id, generation, target_offset, status, failure_class, created_at, completed_at)
			VALUES (?, ?, ?, ?, 'accepted', '', ?, '')
			ON CONFLICT(receipt_id) DO UPDATE SET target_offset=MAX(ingestion_receipts.target_offset, excluded.target_offset)`,
			input.ReceiptID, input.SourceID, input.Generation, input.TargetOffset, formatTime(input.Now))
		if err != nil {
			return IngestionWork{}, err
		}
	}
	work, err := getIngestionWork(tx, input.SourceID, input.Generation)
	if err != nil {
		return IngestionWork{}, err
	}
	if err := tx.Commit(); err != nil {
		return IngestionWork{}, err
	}
	return work, nil
}

func (s *Store) GetIngestionWork(sourceID string, generation int64) (IngestionWork, error) {
	return getIngestionWork(s.db, sourceID, generation)
}

func (s *Store) GetIngestionCheckpoint(sourceID string, generation int64) (IngestionCheckpoint, error) {
	return getIngestionCheckpoint(s.db, sourceID, generation)
}

func (s *Store) AcquireSchedulerLease(ownerID string, now time.Time, ttl time.Duration) (SchedulerLease, error) {
	if strings.TrimSpace(ownerID) == "" || ttl <= 0 {
		return SchedulerLease{}, fmt.Errorf("invalid scheduler lease")
	}
	if now.IsZero() {
		now = time.Now()
	}
	tx, err := s.db.Begin()
	if err != nil {
		return SchedulerLease{}, err
	}
	defer tx.Rollback()
	if _, err := tx.Exec(`UPDATE scheduler_lease SET fencing_token=fencing_token WHERE singleton=1`); err != nil {
		return SchedulerLease{}, err
	}
	lease, err := getSchedulerLease(tx)
	switch {
	case errors.Is(err, sql.ErrNoRows):
		lease = SchedulerLease{OwnerID: ownerID, FencingToken: 1}
	case err != nil:
		return SchedulerLease{}, err
	case lease.OwnerID == ownerID && lease.ExpiresAt.After(now):
		// A heartbeat by the current owner keeps its fencing token.
	case lease.ExpiresAt.After(now):
		return SchedulerLease{}, ErrLeaseHeld
	default:
		lease.OwnerID = ownerID
		lease.FencingToken++
	}
	lease.HeartbeatAt = now
	lease.ExpiresAt = now.Add(ttl)
	_, err = tx.Exec(`INSERT INTO scheduler_lease(singleton, owner_id, fencing_token, expires_at, heartbeat_at)
		VALUES (1, ?, ?, ?, ?)
		ON CONFLICT(singleton) DO UPDATE SET owner_id=excluded.owner_id, fencing_token=excluded.fencing_token, expires_at=excluded.expires_at, heartbeat_at=excluded.heartbeat_at`,
		lease.OwnerID, lease.FencingToken, formatTime(lease.ExpiresAt), formatTime(lease.HeartbeatAt))
	if err != nil {
		return SchedulerLease{}, err
	}
	if err := tx.Commit(); err != nil {
		return SchedulerLease{}, err
	}
	return lease, nil
}

func (s *Store) CommitIngestionSlice(input SliceCommit, write func(*sql.Tx) error) error {
	if input.Now.IsZero() {
		input.Now = time.Now()
	}
	if input.NextOffset < input.ExpectedOffset || len(input.ParserState) > MaxParserStateBytes || input.ParserStateVersion < 0 {
		return fmt.Errorf("%w: invalid checkpoint transition", ErrStateConflict)
	}
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err := tx.Exec(`UPDATE scheduler_lease SET fencing_token=fencing_token WHERE singleton=1`); err != nil {
		return err
	}
	if err := requireFence(tx, input.LeaseOwner, input.FencingToken, input.Now); err != nil {
		return err
	}
	cp, err := getIngestionCheckpoint(tx, input.SourceID, input.Generation)
	if errors.Is(err, sql.ErrNoRows) {
		return ErrGenerationMismatch
	}
	if err != nil {
		return err
	}
	if cp.CommittedOffset != input.ExpectedOffset {
		return fmt.Errorf("%w: checkpoint is %d, expected %d", ErrStateConflict, cp.CommittedOffset, input.ExpectedOffset)
	}
	work, err := getIngestionWork(tx, input.SourceID, input.Generation)
	if errors.Is(err, sql.ErrNoRows) {
		return ErrGenerationMismatch
	}
	if err != nil {
		return err
	}
	if work.State == WorkQuarantined || input.NextOffset > work.TargetOffset || input.VisibilityWatermark < cp.VisibilityWatermark {
		return ErrStateConflict
	}
	if write != nil {
		if err := write(tx); err != nil {
			return err
		}
	}
	_, err = tx.Exec(`UPDATE ingestion_checkpoints SET committed_offset=?, parser_state_version=?, parser_state=?, visibility_watermark=?, updated_at=?
		WHERE source_id=? AND generation=? AND committed_offset=?`, input.NextOffset, input.ParserStateVersion, input.ParserState,
		input.VisibilityWatermark, formatTime(input.Now), input.SourceID, input.Generation, input.ExpectedOffset)
	if err != nil {
		return err
	}
	_, err = tx.Exec(`UPDATE consumer_visibility SET watermark=MAX(watermark, ?), committed_at=? WHERE singleton=1`, input.VisibilityWatermark, formatTime(input.Now))
	if err != nil {
		return err
	}
	state := WorkPending
	if input.Complete || input.NextOffset >= work.TargetOffset {
		state = WorkComplete
	}
	_, err = tx.Exec(`UPDATE ingestion_work SET state=?, active_fence=?, failure_class='', updated_at=? WHERE source_id=? AND generation=?`,
		state, input.FencingToken, formatTime(input.Now), input.SourceID, input.Generation)
	if err != nil {
		return err
	}
	if state == WorkComplete {
		result, err := tx.Exec(`UPDATE ingestion_receipts SET status='completed', completed_at=?
			WHERE source_id=? AND generation=? AND status='accepted' AND target_offset <= ?`, formatTime(input.Now), input.SourceID, input.Generation, input.NextOffset)
		if err != nil {
			return err
		}
		if n, _ := result.RowsAffected(); input.ReceiptID != "" && n == 0 {
			return fmt.Errorf("%w: receipt not found or target not reached", ErrStateConflict)
		}
	}
	return tx.Commit()
}

func (s *Store) QuarantineSource(input QuarantineRequest) error {
	if input.Now.IsZero() {
		input.Now = time.Now()
	}
	if strings.TrimSpace(input.FailureClass) == "" {
		return fmt.Errorf("failure class is required")
	}
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err := tx.Exec(`UPDATE scheduler_lease SET fencing_token=fencing_token WHERE singleton=1`); err != nil {
		return err
	}
	if err := requireFence(tx, input.LeaseOwner, input.FencingToken, input.Now); err != nil {
		return err
	}
	cp, err := getIngestionCheckpoint(tx, input.SourceID, input.Generation)
	if errors.Is(err, sql.ErrNoRows) {
		return ErrGenerationMismatch
	}
	if err != nil {
		return err
	}
	if cp.CommittedOffset != input.ExpectedOffset {
		return ErrStateConflict
	}
	result, err := tx.Exec(`UPDATE ingestion_work SET state='quarantined', retry_count=retry_count+1, failure_class=?, active_fence=?, updated_at=?
		WHERE source_id=? AND generation=?`, input.FailureClass, input.FencingToken, formatTime(input.Now), input.SourceID, input.Generation)
	if err != nil {
		return err
	}
	if n, _ := result.RowsAffected(); n != 1 {
		return ErrGenerationMismatch
	}
	_, err = tx.Exec(`UPDATE ingestion_sources SET state='quarantined', updated_at=? WHERE source_id=? AND generation=?`, formatTime(input.Now), input.SourceID, input.Generation)
	if err != nil {
		return err
	}
	_, err = tx.Exec(`UPDATE ingestion_receipts SET status='quarantined', failure_class=?, completed_at=?
		WHERE source_id=? AND generation=? AND status='accepted'`, input.FailureClass, formatTime(input.Now), input.SourceID, input.Generation)
	if err != nil {
		return err
	}
	return tx.Commit()
}

func (s *Store) ResumeSource(sourceID string, expectedGeneration int64, now time.Time) error {
	if now.IsZero() {
		now = time.Now()
	}
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	var latest sql.NullInt64
	if err := tx.QueryRow(`SELECT MAX(generation) FROM ingestion_sources WHERE source_id=?`, sourceID).Scan(&latest); err != nil {
		return err
	}
	if !latest.Valid || latest.Int64 != expectedGeneration {
		return ErrGenerationMismatch
	}
	result, err := tx.Exec(`UPDATE ingestion_work SET state='pending', retry_count=0, failure_class='', active_fence=0, updated_at=?
		WHERE source_id=? AND generation=? AND state='quarantined'`, formatTime(now), sourceID, expectedGeneration)
	if err != nil {
		return err
	}
	if n, _ := result.RowsAffected(); n == 0 {
		var state string
		if err := tx.QueryRow(`SELECT state FROM ingestion_work WHERE source_id=? AND generation=?`, sourceID, expectedGeneration).Scan(&state); err != nil {
			return ErrGenerationMismatch
		}
		if state != string(WorkPending) {
			return ErrStateConflict
		}
	}
	_, err = tx.Exec(`UPDATE ingestion_sources SET state='active', updated_at=? WHERE source_id=? AND generation=?`, formatTime(now), sourceID, expectedGeneration)
	if err != nil {
		return err
	}
	return tx.Commit()
}

func (s *Store) GetIngestionReceipt(receiptID string) (IngestionReceipt, error) {
	var receipt IngestionReceipt
	var status, created, completed string
	err := s.db.QueryRow(`SELECT receipt_id, source_id, generation, target_offset, status, failure_class, created_at, completed_at
		FROM ingestion_receipts WHERE receipt_id=?`, receiptID).Scan(&receipt.ReceiptID, &receipt.SourceID, &receipt.Generation,
		&receipt.TargetOffset, &status, &receipt.FailureClass, &created, &completed)
	receipt.Status = ReceiptStatus(status)
	receipt.CreatedAt = parseTime(created)
	receipt.CompletedAt = parseTime(completed)
	return receipt, err
}

type rowQuerier interface {
	QueryRow(query string, args ...any) *sql.Row
}

func getIngestionWork(q rowQuerier, sourceID string, generation int64) (IngestionWork, error) {
	var work IngestionWork
	var state, created, updated string
	err := q.QueryRow(`SELECT source_id, generation, work_class, target_offset, state, retry_count, failure_class, active_fence, created_at, updated_at
		FROM ingestion_work WHERE source_id=? AND generation=?`, sourceID, generation).Scan(&work.SourceID, &work.Generation, &work.Class,
		&work.TargetOffset, &state, &work.RetryCount, &work.FailureClass, &work.ActiveFence, &created, &updated)
	work.State = WorkState(state)
	work.CreatedAt = parseTime(created)
	work.UpdatedAt = parseTime(updated)
	return work, err
}

func getIngestionCheckpoint(q rowQuerier, sourceID string, generation int64) (IngestionCheckpoint, error) {
	var cp IngestionCheckpoint
	var updated string
	err := q.QueryRow(`SELECT source_id, generation, committed_offset, parser_state_version, parser_state, visibility_watermark, updated_at
		FROM ingestion_checkpoints WHERE source_id=? AND generation=?`, sourceID, generation).Scan(&cp.SourceID, &cp.Generation,
		&cp.CommittedOffset, &cp.ParserStateVersion, &cp.ParserState, &cp.VisibilityWatermark, &updated)
	cp.UpdatedAt = parseTime(updated)
	return cp, err
}

func getSchedulerLease(q rowQuerier) (SchedulerLease, error) {
	var lease SchedulerLease
	var expires, heartbeat string
	err := q.QueryRow(`SELECT owner_id, fencing_token, expires_at, heartbeat_at FROM scheduler_lease WHERE singleton=1`).Scan(
		&lease.OwnerID, &lease.FencingToken, &expires, &heartbeat)
	lease.ExpiresAt = parseTime(expires)
	lease.HeartbeatAt = parseTime(heartbeat)
	return lease, err
}

func requireFence(q rowQuerier, owner string, token int64, now time.Time) error {
	lease, err := getSchedulerLease(q)
	if err != nil || lease.OwnerID != owner || lease.FencingToken != token || !lease.ExpiresAt.After(now) {
		return ErrStaleFence
	}
	return nil
}
