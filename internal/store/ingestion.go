package store

import (
	"bytes"
	"database/sql"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
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
	WorkPending       WorkState = "pending"
	WorkRunning       WorkState = "running"
	WorkComplete      WorkState = "complete"
	WorkWaitingAppend WorkState = "waiting_append"
	WorkQuarantined   WorkState = "quarantined"
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
	AwaitingAppend      bool
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

type ConsumerVisibility struct {
	Watermark   int64
	CommittedAt time.Time
}

// ParsedSlice is the normalized, bounded output of one parser slice. Keeping
// this type in store avoids coupling the durable queue to a concrete adapter.
type ParsedSlice struct {
	Session     model.Session
	Turns       []model.Turn
	Actions     []model.Action
	Corrections []model.Correction
	CursorHash  string
}

// RunnableIngestion joins the durable work identity with its immutable source
// reference. It is returned only after the caller proves it owns the current
// scheduler fence.
type RunnableIngestion struct {
	Work   IngestionWork
	Source SourceGeneration
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
				WHEN ingestion_work.state = 'waiting_append' AND excluded.target_offset <= ingestion_work.target_offset THEN 'waiting_append'
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
		_, err = tx.Exec(`UPDATE ingestion_receipts SET status='completed', completed_at=?
			WHERE receipt_id=? AND target_offset <= COALESCE((SELECT committed_offset FROM ingestion_checkpoints
				WHERE source_id=? AND generation=?), 0)`, formatTime(input.Now), input.ReceiptID, input.SourceID, input.Generation)
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

// InitializeIngestionCheckpoint installs the structural baseline for a newly
// observed generation. It is intentionally compare-and-set: reconciliation
// may race across processes but can never move an established checkpoint.
func (s *Store) InitializeIngestionCheckpoint(sourceID string, generation, offset int64, parserVersion int, parserState []byte, now time.Time) error {
	if generation <= 0 || offset < 0 || parserVersion < 0 || len(parserState) > MaxParserStateBytes {
		return ErrStateConflict
	}
	if now.IsZero() {
		now = time.Now()
	}
	result, err := s.db.Exec(`UPDATE ingestion_checkpoints SET committed_offset=?, parser_state_version=?, parser_state=?, updated_at=?
		WHERE source_id=? AND generation=? AND committed_offset=0 AND parser_state_version=0 AND length(parser_state)=0`,
		offset, parserVersion, parserState, formatTime(now), sourceID, generation)
	if err != nil {
		return err
	}
	if n, _ := result.RowsAffected(); n == 0 {
		cp, getErr := s.GetIngestionCheckpoint(sourceID, generation)
		if getErr != nil {
			return getErr
		}
		if cp.CommittedOffset != offset && cp.CommittedOffset == 0 {
			return ErrStateConflict
		}
	}
	return nil
}

// ClaimNextIngestionWork chooses exactly one runnable row in strict class
// order. The lease and the claim are checked in the same transaction so a
// stale process cannot begin new work after losing ownership.
func (s *Store) ClaimNextIngestionWork(owner string, fence int64, now time.Time) (RunnableIngestion, error) {
	if now.IsZero() {
		now = time.Now()
	}
	tx, err := s.db.Begin()
	if err != nil {
		return RunnableIngestion{}, err
	}
	defer tx.Rollback()
	if _, err := tx.Exec(`UPDATE scheduler_lease SET fencing_token=fencing_token WHERE singleton=1`); err != nil {
		return RunnableIngestion{}, err
	}
	if err := requireFence(tx, owner, fence, now); err != nil {
		return RunnableIngestion{}, err
	}
	var item RunnableIngestion
	var workState, sourceState, workCreated, workUpdated, sourceCreated, sourceUpdated string
	err = tx.QueryRow(`SELECT w.source_id, w.generation, w.work_class, w.target_offset, w.state,
		w.retry_count, w.failure_class, w.active_fence, w.created_at, w.updated_at,
		s.agent, s.source_ref, s.state, s.created_at, s.updated_at
		FROM ingestion_work w JOIN ingestion_sources s USING(source_id, generation)
		WHERE w.state='pending' AND s.state='active'
		ORDER BY w.work_class ASC, w.updated_at ASC, w.source_id ASC LIMIT 1`).Scan(
		&item.Work.SourceID, &item.Work.Generation, &item.Work.Class, &item.Work.TargetOffset, &workState,
		&item.Work.RetryCount, &item.Work.FailureClass, &item.Work.ActiveFence, &workCreated, &workUpdated,
		&item.Source.Agent, &item.Source.SourceRef, &sourceState, &sourceCreated, &sourceUpdated)
	if err != nil {
		return RunnableIngestion{}, err
	}
	item.Work.State = WorkRunning
	item.Work.CreatedAt, item.Work.UpdatedAt = parseTime(workCreated), parseTime(workUpdated)
	item.Source.SourceID, item.Source.Generation = item.Work.SourceID, item.Work.Generation
	item.Source.State = SourceState(sourceState)
	item.Source.CreatedAt, item.Source.UpdatedAt = parseTime(sourceCreated), parseTime(sourceUpdated)
	result, err := tx.Exec(`UPDATE ingestion_work SET state='running', active_fence=?, updated_at=?
		WHERE source_id=? AND generation=? AND state='pending'`, fence, formatTime(now), item.Work.SourceID, item.Work.Generation)
	if err != nil {
		return RunnableIngestion{}, err
	}
	if n, _ := result.RowsAffected(); n != 1 {
		return RunnableIngestion{}, ErrStateConflict
	}
	return item, tx.Commit()
}

func (s *Store) HasPendingLiveWork() (bool, error) {
	var n int
	err := s.db.QueryRow(`SELECT COUNT(*) FROM ingestion_work WHERE state='pending' AND work_class=?`, WorkLive).Scan(&n)
	return n > 0, err
}

func (s *Store) RecoverStaleRunningWork(owner string, fence int64, now time.Time) error {
	if now.IsZero() {
		now = time.Now()
	}
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if err := requireFence(tx, owner, fence, now); err != nil {
		return err
	}
	if _, err := tx.Exec(`UPDATE ingestion_work SET state='pending', updated_at=?
		WHERE state='running' AND active_fence<>?`, formatTime(now), fence); err != nil {
		return err
	}
	return tx.Commit()
}

// CommitParsedIngestionSlice persists normalized records, checkpoint/parser
// state, visibility watermark, receipt completion, and queue state atomically.
func (s *Store) CommitParsedIngestionSlice(input SliceCommit, parsed ParsedSlice) error {
	return s.CommitIngestionSlice(input, func(tx *sql.Tx) error {
		if err := writeParsedEntities(tx, parsed); err != nil {
			return err
		}
		sess := parsed.Session
		if sess.SourcePath != "" {
			return upsertCursorTx(tx, CursorRow{SourcePath: sess.SourcePath, Agent: sess.Agent, LastOffset: input.NextOffset,
				LastHash: parsed.CursorHash, LastSyncedAt: input.Now})
		}
		return nil
	})
}

func writeParsedEntities(tx *sql.Tx, parsed ParsedSlice) error {
	sess := parsed.Session
	if sess.ID != "" {
		if _, err := tx.Exec(`INSERT INTO sessions(id, agent, project, source_path, source_hash, started_at, updated_at)
			VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET source_hash=excluded.source_hash, updated_at=excluded.updated_at`,
			sess.ID, sess.Agent, sess.Project, sess.SourcePath, sess.SourceHash, formatTime(sess.StartedAt), formatTime(sess.UpdatedAt)); err != nil {
			return err
		}
	}
	for _, turn := range parsed.Turns {
		if _, err := tx.Exec(`INSERT OR IGNORE INTO turns(id, session_id, turn_index, role, event_type, created_at) VALUES (?, ?, ?, ?, ?, ?)`,
			turn.ID, turn.SessionID, turn.TurnIndex, turn.Role, turn.EventType, formatTime(turn.CreatedAt)); err != nil {
			return err
		}
	}
	for _, action := range parsed.Actions {
		if _, err := tx.Exec(`INSERT OR IGNORE INTO actions(id, turn_id, tool_name, command, file_path, excerpt) VALUES (?, ?, ?, ?, ?, ?)`,
			action.ID, action.TurnID, action.ToolName, action.Command, action.FilePath, action.Excerpt); err != nil {
			return err
		}
	}
	for _, correction := range parsed.Corrections {
		if _, err := tx.Exec(`INSERT OR IGNORE INTO corrections(id, session_id, turn_id, action_id, hash, normalized, excerpt, agent, project, created_at)
			VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`, correction.ID, correction.SessionID, correction.TurnID, correction.ActionID,
			correction.Hash, correction.Normalized, correction.Excerpt, correction.Agent, correction.Project, formatTime(correction.CreatedAt)); err != nil {
			return err
		}
	}
	return nil
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
	if input.NextOffset < input.ExpectedOffset || len(input.ParserState) > MaxParserStateBytes || input.ParserStateVersion < 0 || (input.Complete && input.AwaitingAppend) {
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
	var sourceState string
	var latestGeneration sql.NullInt64
	if err := tx.QueryRow(`SELECT state FROM ingestion_sources WHERE source_id=? AND generation=?`, input.SourceID, input.Generation).Scan(&sourceState); err != nil {
		return ErrGenerationMismatch
	}
	if err := tx.QueryRow(`SELECT MAX(generation) FROM ingestion_sources WHERE source_id=?`, input.SourceID).Scan(&latestGeneration); err != nil {
		return err
	}
	if sourceState != string(SourceActive) || !latestGeneration.Valid || latestGeneration.Int64 != input.Generation {
		return ErrGenerationMismatch
	}
	work, err := getIngestionWork(tx, input.SourceID, input.Generation)
	if errors.Is(err, sql.ErrNoRows) {
		return ErrGenerationMismatch
	}
	if err != nil {
		return err
	}
	// A caller can lose the response after SQLite commits. Treat a byte-for-byte
	// replay of that already published checkpoint as success without rewriting
	// entities or queue state.
	if cp.CommittedOffset == input.NextOffset && cp.ParserStateVersion == input.ParserStateVersion &&
		bytes.Equal(cp.ParserState, input.ParserState) && cp.VisibilityWatermark == input.VisibilityWatermark &&
		work.State == WorkComplete {
		return tx.Commit()
	}
	if cp.CommittedOffset != input.ExpectedOffset {
		return fmt.Errorf("%w: checkpoint is %d, expected %d", ErrStateConflict, cp.CommittedOffset, input.ExpectedOffset)
	}
	if work.State != WorkRunning || work.ActiveFence != input.FencingToken {
		return fmt.Errorf("%w: work is not claimed by fence", ErrStateConflict)
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
	if input.NextOffset >= work.TargetOffset {
		state = WorkComplete
	} else if input.AwaitingAppend {
		state = WorkWaitingAppend
	} else if input.Complete {
		return fmt.Errorf("%w: completed slice is below target", ErrStateConflict)
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

func (s *Store) GetConsumerVisibility() (ConsumerVisibility, error) {
	var visibility ConsumerVisibility
	var committed string
	err := s.db.QueryRow(`SELECT watermark, committed_at FROM consumer_visibility WHERE singleton=1`).Scan(&visibility.Watermark, &committed)
	visibility.CommittedAt = parseTime(committed)
	return visibility, err
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
