package store

import (
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	_ "github.com/mattn/go-sqlite3"

	"github.com/hippoom/agbox/internal/model"
	proposestate "github.com/hippoom/agbox/internal/propose/state"
)

type Store struct {
	db   *sql.DB
	path string
}

const candidateSelectCols = `id, fingerprint, name, description, rule_text, semantic_key, source_kind, state, event_count, project_count, source_count, first_seen, last_seen, confidence, version, updated_at, proposed_at, snoozed_until, skill_path`

func DefaultPath() (string, error) {
	if p := os.Getenv("AGBOX_DB"); p != "" {
		return p, nil
	}
	home := os.Getenv("AGBOX_HOME")
	if home == "" {
		userHome, err := os.UserHomeDir()
		if err != nil {
			return "", err
		}
		home = filepath.Join(userHome, ".agbox")
	}
	return filepath.Join(home, "agbox.db"), nil
}

func Open(path string) (*Store, error) {
	if path == "" {
		var err error
		path, err = DefaultPath()
		if err != nil {
			return nil, err
		}
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, err
	}
	db, err := sql.Open("sqlite3", path+"?_busy_timeout=5000&_foreign_keys=on")
	if err != nil {
		return nil, err
	}
	s := &Store{db: db, path: path}
	if err := s.migrate(); err != nil {
		db.Close()
		return nil, err
	}
	return s, nil
}

func (s *Store) Close() error {
	if s == nil || s.db == nil {
		return nil
	}
	return s.db.Close()
}

func (s *Store) Path() string {
	return s.path
}

func (s *Store) migrate() error {
	stmts := []string{
		`PRAGMA journal_mode=WAL`,
		`PRAGMA synchronous=NORMAL`,
		`CREATE TABLE IF NOT EXISTS events (
			id TEXT PRIMARY KEY,
			hash TEXT NOT NULL,
			normalized TEXT NOT NULL,
			source TEXT NOT NULL,
			agent TEXT NOT NULL,
			project TEXT NOT NULL,
			excerpt TEXT NOT NULL,
			raw TEXT NOT NULL,
			raw_stored INTEGER NOT NULL DEFAULT 0,
			created_at TEXT NOT NULL
		)`,
		`CREATE INDEX IF NOT EXISTS idx_events_hash ON events(hash)`,
		`CREATE INDEX IF NOT EXISTS idx_events_created ON events(created_at)`,
		`CREATE TABLE IF NOT EXISTS candidates (
			id TEXT PRIMARY KEY,
			fingerprint TEXT NOT NULL UNIQUE,
			name TEXT NOT NULL,
			description TEXT NOT NULL,
			rule_text TEXT NOT NULL,
			state TEXT NOT NULL,
			event_count INTEGER NOT NULL,
			project_count INTEGER NOT NULL,
			source_count INTEGER NOT NULL,
			first_seen TEXT NOT NULL,
			last_seen TEXT NOT NULL,
			confidence TEXT NOT NULL,
			version INTEGER NOT NULL,
			updated_at TEXT NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS candidate_events (
			candidate_id TEXT NOT NULL,
			event_id TEXT NOT NULL,
			PRIMARY KEY(candidate_id, event_id),
			FOREIGN KEY(candidate_id) REFERENCES candidates(id) ON DELETE CASCADE,
			FOREIGN KEY(event_id) REFERENCES events(id) ON DELETE CASCADE
		)`,
		`CREATE TABLE IF NOT EXISTS exports (
			id TEXT PRIMARY KEY,
			candidate_id TEXT NOT NULL,
			target TEXT NOT NULL,
			path TEXT NOT NULL,
			status TEXT NOT NULL,
			plan_json TEXT NOT NULL,
			backup_path TEXT NOT NULL,
			before_hash TEXT NOT NULL,
			after_hash TEXT NOT NULL,
			applied_at TEXT NOT NULL,
			rolled_back_at TEXT NOT NULL,
			created_at TEXT NOT NULL,
			FOREIGN KEY(candidate_id) REFERENCES candidates(id) ON DELETE CASCADE
		)`,
	}
	for _, stmt := range stmts {
		if _, err := s.db.Exec(stmt); err != nil {
			return err
		}
	}
	if err := migrateV2(s.db); err != nil {
		return err
	}
	if err := migrateV3(s.db); err != nil {
		return err
	}
	if err := migrateV4(s.db); err != nil {
		return err
	}
	if err := migrateV5(s.db); err != nil {
		return err
	}
	return migrateV6(s.db)
}

func (s *Store) TableExists(name string) bool {
	var n int
	_ = s.db.QueryRow(`SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?`, name).Scan(&n)
	return n == 1
}

func (s *Store) InsertEvent(e model.Event) error {
	_, err := s.db.Exec(`INSERT INTO events
		(id, hash, normalized, source, agent, project, excerpt, raw, raw_stored, created_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		e.ID, e.Hash, e.Normalized, e.Source, e.Agent, e.Project, e.Excerpt, e.Raw, boolInt(e.RawStored), formatTime(e.CreatedAt))
	return err
}

func (s *Store) ListEvents() ([]model.Event, error) {
	rows, err := s.db.Query(`SELECT id, hash, normalized, source, agent, project, excerpt, raw, raw_stored, created_at FROM events ORDER BY created_at ASC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []model.Event
	for rows.Next() {
		var e model.Event
		var rawStored int
		var created string
		if err := rows.Scan(&e.ID, &e.Hash, &e.Normalized, &e.Source, &e.Agent, &e.Project, &e.Excerpt, &e.Raw, &rawStored, &created); err != nil {
			return nil, err
		}
		e.RawStored = rawStored == 1
		e.CreatedAt = parseTime(created)
		out = append(out, e)
	}
	return out, rows.Err()
}

func (s *Store) EventsForCandidate(candidateID string) ([]model.Event, error) {
	rows, err := s.db.Query(`SELECT e.id, e.hash, e.normalized, e.source, e.agent, e.project, e.excerpt, e.raw, e.raw_stored, e.created_at
		FROM events e
		JOIN candidate_events ce ON ce.event_id = e.id
		WHERE ce.candidate_id = ?
		ORDER BY e.created_at ASC`, candidateID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []model.Event
	for rows.Next() {
		var e model.Event
		var rawStored int
		var created string
		if err := rows.Scan(&e.ID, &e.Hash, &e.Normalized, &e.Source, &e.Agent, &e.Project, &e.Excerpt, &e.Raw, &rawStored, &created); err != nil {
			return nil, err
		}
		e.RawStored = rawStored == 1
		e.CreatedAt = parseTime(created)
		out = append(out, e)
	}
	return out, rows.Err()
}

func (s *Store) UpsertCandidate(c model.Candidate, eventIDs, correctionIDs []string) error {
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()

	var existing model.Candidate
	var hasExisting bool
	row := tx.QueryRow(`SELECT `+candidateSelectCols+` FROM candidates WHERE fingerprint = ?`, c.Fingerprint)
	if scanned, scanErr := scanCandidate(row); scanErr == nil {
		existing = scanned
		hasExisting = true
	} else if !errors.Is(scanErr, sql.ErrNoRows) {
		return scanErr
	}
	if !hasExisting {
		linked, found, linkErr := candidateLinkedToScanInputs(tx, eventIDs, correctionIDs)
		if linkErr != nil {
			return linkErr
		}
		if found {
			existing = linked
			hasExisting = true
			c.Fingerprint = existing.Fingerprint
		}
	}

	now := time.Now()
	c.UpdatedAt = now
	if c.SourceKind == "" {
		if len(correctionIDs) > 0 {
			c.SourceKind = model.CandidateSourceCorrection
		} else {
			c.SourceKind = model.CandidateSourcePromptPattern
		}
	}
	if hasExisting {
		merged := proposestate.MergeOnScan(existing, c)
		c.State = merged.State
		c.Version = merged.Version
		c.ID = existing.ID
	} else {
		if c.State == "" {
			c.State = model.CandidatePending
		}
		if c.Version == 0 {
			c.Version = 1
		}
	}

	_, err = tx.Exec(`INSERT INTO candidates
		(id, fingerprint, name, description, rule_text, semantic_key, source_kind, state, event_count, project_count, source_count, first_seen, last_seen, confidence, version, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(fingerprint) DO UPDATE SET
			name=excluded.name,
			description=excluded.description,
			rule_text=excluded.rule_text,
			semantic_key=excluded.semantic_key,
			source_kind=excluded.source_kind,
			event_count=excluded.event_count,
			project_count=excluded.project_count,
			source_count=excluded.source_count,
			first_seen=excluded.first_seen,
			last_seen=excluded.last_seen,
			confidence=excluded.confidence,
			version=excluded.version,
			updated_at=excluded.updated_at`,
		c.ID, c.Fingerprint, c.Name, c.Description, c.RuleText, c.SemanticKey, c.SourceKind, c.State, c.EventCount, c.ProjectCount, c.SourceCount,
		formatTime(c.FirstSeen), formatTime(c.LastSeen), c.Confidence, c.Version, formatTime(c.UpdatedAt))
	if err != nil {
		return err
	}
	for _, eventID := range eventIDs {
		if _, err := tx.Exec(`INSERT OR IGNORE INTO candidate_events(candidate_id, event_id) VALUES (?, ?)`, c.ID, eventID); err != nil {
			return err
		}
	}
	for _, correctionID := range correctionIDs {
		if _, err := tx.Exec(`INSERT OR IGNORE INTO candidate_corrections(candidate_id, correction_id) VALUES (?, ?)`, c.ID, correctionID); err != nil {
			return err
		}
	}
	return tx.Commit()
}

func candidateLinkedToScanInputs(tx *sql.Tx, eventIDs, correctionIDs []string) (model.Candidate, bool, error) {
	if len(correctionIDs) > 0 {
		c, found, err := candidateLinkedByIDs(tx, "candidate_corrections", "correction_id", correctionIDs)
		if err != nil || found {
			return c, found, err
		}
	}
	if len(eventIDs) > 0 {
		return candidateLinkedByIDs(tx, "candidate_events", "event_id", eventIDs)
	}
	return model.Candidate{}, false, nil
}

func candidateLinkedByIDs(tx *sql.Tx, table, idColumn string, ids []string) (model.Candidate, bool, error) {
	placeholders := make([]string, len(ids))
	args := make([]any, len(ids))
	for i, id := range ids {
		placeholders[i] = "?"
		args[i] = id
	}
	query := `SELECT ` + candidateSelectCols + `
		FROM candidates c
		JOIN ` + table + ` link ON link.candidate_id = c.id
		WHERE link.` + idColumn + ` IN (` + strings.Join(placeholders, ",") + `)
		ORDER BY c.updated_at DESC
		LIMIT 1`
	c, err := scanCandidate(tx.QueryRow(query, args...))
	if errors.Is(err, sql.ErrNoRows) {
		return model.Candidate{}, false, nil
	}
	if err != nil {
		return model.Candidate{}, false, err
	}
	return c, true, nil
}

func (s *Store) ListCandidates(state string) ([]model.Candidate, error) {
	query := `SELECT ` + candidateSelectCols + ` FROM candidates`
	var args []any
	if state != "" {
		query += ` WHERE state = ?`
		args = append(args, state)
	}
	query += ` ORDER BY event_count DESC, last_seen DESC`
	rows, err := s.db.Query(query, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []model.Candidate
	for rows.Next() {
		c, err := scanCandidate(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, c)
	}
	return out, rows.Err()
}

func (s *Store) GetCandidate(id string) (model.Candidate, error) {
	row := s.db.QueryRow(`SELECT `+candidateSelectCols+` FROM candidates WHERE id = ?`, id)
	c, err := scanCandidate(row)
	if errors.Is(err, sql.ErrNoRows) {
		return model.Candidate{}, fmt.Errorf("candidate not found: %s", id)
	}
	return c, err
}

func (s *Store) GetProposedCandidate(id string) (model.Candidate, error) {
	row := s.db.QueryRow(`SELECT `+candidateSelectCols+` FROM candidates WHERE id = ? AND state = ?`, id, model.CandidateProposed)
	c, err := scanCandidate(row)
	if errors.Is(err, sql.ErrNoRows) {
		return model.Candidate{}, fmt.Errorf("proposed candidate not found: %s", id)
	}
	return c, err
}

func (s *Store) SetCandidateState(id string, state model.CandidateState, name string) error {
	return s.UpdateCandidateMeta(id, CandidateMetaUpdate{State: state, Name: name})
}

type CandidateMetaUpdate struct {
	State        model.CandidateState
	Name         string
	ProposedAt   *time.Time
	SnoozedUntil *time.Time
	SkillPath    string
}

func (s *Store) UpdateCandidateMeta(id string, upd CandidateMetaUpdate) error {
	now := time.Now()
	sets, args := candidateMetaUpdateParts(upd, now)
	args = append(args, id)
	_, err := s.db.Exec(`UPDATE candidates SET `+strings.Join(sets, ", ")+` WHERE id = ?`, args...)
	return err
}

func (s *Store) UpdateCandidateMetaIfState(id string, expected model.CandidateState, upd CandidateMetaUpdate) (bool, error) {
	now := time.Now()
	sets, args := candidateMetaUpdateParts(upd, now)
	args = append(args, id, expected)
	result, err := s.db.Exec(`UPDATE candidates SET `+strings.Join(sets, ", ")+` WHERE id = ? AND state = ?`, args...)
	if err != nil {
		return false, err
	}
	rows, err := result.RowsAffected()
	if err != nil {
		return false, err
	}
	return rows > 0, nil
}

func candidateMetaUpdateParts(upd CandidateMetaUpdate, now time.Time) ([]string, []any) {
	sets := []string{"version = version + 1", "updated_at = ?"}
	args := []any{formatTime(now)}
	if upd.State != "" {
		sets = append(sets, "state = ?")
		args = append(args, upd.State)
	}
	if upd.Name != "" {
		sets = append(sets, "name = ?")
		args = append(args, upd.Name)
	}
	if upd.ProposedAt != nil {
		sets = append(sets, "proposed_at = ?")
		args = append(args, formatTime(*upd.ProposedAt))
	}
	if upd.SnoozedUntil != nil {
		sets = append(sets, "snoozed_until = ?")
		args = append(args, formatTime(*upd.SnoozedUntil))
	}
	if upd.SkillPath != "" {
		sets = append(sets, "skill_path = ?")
		args = append(args, upd.SkillPath)
	}
	return sets, args
}

func (s *Store) ListCandidatesByState(states ...model.CandidateState) ([]model.Candidate, error) {
	if len(states) == 0 {
		return s.ListCandidates("")
	}
	placeholders := make([]string, len(states))
	args := make([]any, len(states))
	for i, st := range states {
		placeholders[i] = "?"
		args[i] = string(st)
	}
	query := `SELECT ` + candidateSelectCols + ` FROM candidates WHERE state IN (` + strings.Join(placeholders, ",") + `) ORDER BY event_count DESC, last_seen DESC`
	rows, err := s.db.Query(query, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []model.Candidate
	for rows.Next() {
		c, err := scanCandidate(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, c)
	}
	return out, rows.Err()
}

func (s *Store) RecordReplayApplication(app model.ReplayApplication) (model.ReplayApplication, error) {
	now := time.Now()
	if app.ID == "" {
		app.ID = replayApplicationID(app.CandidateID, now)
	}
	if app.AppliedAt.IsZero() {
		app.AppliedAt = now
	}
	if app.CreatedAt.IsZero() {
		app.CreatedAt = now
	}

	tx, err := s.db.Begin()
	if err != nil {
		return model.ReplayApplication{}, err
	}
	defer tx.Rollback()

	var state string
	if err := tx.QueryRow(`SELECT state FROM candidates WHERE id = ?`, app.CandidateID).Scan(&state); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return model.ReplayApplication{}, fmt.Errorf("candidate not found: %s", app.CandidateID)
		}
		return model.ReplayApplication{}, err
	}
	currentState := model.CandidateState(state)

	if _, err := tx.Exec(`INSERT INTO replay_applications
		(id, candidate_id, agent, project, prompt_hash, prompt_excerpt, applied_at, created_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
		app.ID, app.CandidateID, app.Agent, app.Project, app.PromptHash, app.PromptExcerpt,
		formatTime(app.AppliedAt), formatTime(app.CreatedAt)); err != nil {
		return model.ReplayApplication{}, err
	}
	if canMarkAppliedOnce(currentState) {
		result, err := tx.Exec(`UPDATE candidates SET state = ?, version = version + 1, updated_at = ? WHERE id = ? AND state = ?`,
			model.CandidateAppliedOnce, formatTime(now), app.CandidateID, currentState)
		if err != nil {
			return model.ReplayApplication{}, err
		}
		rows, err := result.RowsAffected()
		if err != nil {
			return model.ReplayApplication{}, err
		}
		if rows == 0 {
			return model.ReplayApplication{}, fmt.Errorf("candidate state changed before replay application: %s", app.CandidateID)
		}
	}
	if err := tx.Commit(); err != nil {
		return model.ReplayApplication{}, err
	}
	return app, nil
}

func (s *Store) ListReplayApplications(candidateID string) ([]model.ReplayApplication, error) {
	query := `SELECT id, candidate_id, agent, project, prompt_hash, prompt_excerpt, applied_at, created_at FROM replay_applications`
	var args []any
	if strings.TrimSpace(candidateID) != "" {
		query += ` WHERE candidate_id = ?`
		args = append(args, candidateID)
	}
	query += ` ORDER BY applied_at DESC, created_at DESC`
	rows, err := s.db.Query(query, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []model.ReplayApplication
	for rows.Next() {
		app, err := scanReplayApplication(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, app)
	}
	return out, rows.Err()
}

func (s *Store) LatestReplayApplication(candidateID, project string) (model.ReplayApplication, error) {
	query := `SELECT id, candidate_id, agent, project, prompt_hash, prompt_excerpt, applied_at, created_at
		FROM replay_applications WHERE candidate_id = ?`
	args := []any{candidateID}
	if strings.TrimSpace(project) != "" {
		query += ` AND project = ?`
		args = append(args, project)
	}
	query += ` ORDER BY applied_at DESC, created_at DESC LIMIT 1`
	app, err := scanReplayApplication(s.db.QueryRow(query, args...))
	if errors.Is(err, sql.ErrNoRows) {
		return model.ReplayApplication{}, fmt.Errorf("replay application not found: %s", candidateID)
	}
	return app, err
}

func (s *Store) CreateExport(e model.ExportRecord) error {
	_, err := s.db.Exec(`INSERT INTO exports
		(id, candidate_id, target, path, status, plan_json, backup_path, before_hash, after_hash, applied_at, rolled_back_at, created_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		e.ID, e.CandidateID, e.Target, e.Path, e.Status, e.PlanJSON, e.BackupPath, e.BeforeHash, e.AfterHash,
		formatTime(e.AppliedAt), formatTime(e.RolledBackAt), formatTime(e.CreatedAt))
	return err
}

func (s *Store) UpdateExport(e model.ExportRecord) error {
	_, err := s.db.Exec(`UPDATE exports SET status = ?, plan_json = ?, backup_path = ?, before_hash = ?, after_hash = ?, applied_at = ?, rolled_back_at = ? WHERE id = ?`,
		e.Status, e.PlanJSON, e.BackupPath, e.BeforeHash, e.AfterHash, formatTime(e.AppliedAt), formatTime(e.RolledBackAt), e.ID)
	return err
}

func (s *Store) GetExport(id string) (model.ExportRecord, error) {
	row := s.db.QueryRow(`SELECT id, candidate_id, target, path, status, plan_json, backup_path, before_hash, after_hash, applied_at, rolled_back_at, created_at FROM exports WHERE id = ?`, id)
	e, err := scanExport(row)
	if errors.Is(err, sql.ErrNoRows) {
		return model.ExportRecord{}, fmt.Errorf("export not found: %s", id)
	}
	return e, err
}

func (s *Store) LatestExportForCandidate(candidateID string) (model.ExportRecord, error) {
	row := s.db.QueryRow(`SELECT id, candidate_id, target, path, status, plan_json, backup_path, before_hash, after_hash, applied_at, rolled_back_at, created_at
		FROM exports WHERE candidate_id = ? ORDER BY created_at DESC LIMIT 1`, candidateID)
	return scanExport(row)
}

func (s *Store) ListExports() ([]model.ExportRecord, error) {
	rows, err := s.db.Query(`SELECT id, candidate_id, target, path, status, plan_json, backup_path, before_hash, after_hash, applied_at, rolled_back_at, created_at FROM exports ORDER BY created_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []model.ExportRecord
	for rows.Next() {
		e, err := scanExport(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, e)
	}
	return out, rows.Err()
}

func (s *Store) Stats() (model.StoreStats, error) {
	var stats model.StoreStats
	stats.Path = s.path
	if err := s.db.QueryRow(`SELECT COUNT(*) FROM events`).Scan(&stats.Events); err != nil {
		return stats, err
	}
	if err := s.db.QueryRow(`SELECT COUNT(*) FROM candidates`).Scan(&stats.Candidates); err != nil {
		return stats, err
	}
	if err := s.db.QueryRow(`SELECT COUNT(*) FROM exports`).Scan(&stats.Exports); err != nil {
		return stats, err
	}
	return stats, nil
}

func scanCandidate(scanner interface{ Scan(dest ...any) error }) (model.Candidate, error) {
	var c model.Candidate
	var first, last, updated, proposed, snoozed, skillPath string
	var sourceKind, state string
	err := scanner.Scan(&c.ID, &c.Fingerprint, &c.Name, &c.Description, &c.RuleText, &c.SemanticKey, &sourceKind, &state, &c.EventCount, &c.ProjectCount, &c.SourceCount, &first, &last, &c.Confidence, &c.Version, &updated, &proposed, &snoozed, &skillPath)
	c.SourceKind = model.CandidateSourceKind(sourceKind)
	if c.SourceKind == "" {
		c.SourceKind = model.CandidateSourcePromptPattern
	}
	c.State = model.CandidateState(state)
	c.FirstSeen = parseTime(first)
	c.LastSeen = parseTime(last)
	c.UpdatedAt = parseTime(updated)
	c.ProposedAt = parseTime(proposed)
	c.SnoozedUntil = parseTime(snoozed)
	c.SkillPath = skillPath
	return c, err
}

func scanReplayApplication(scanner interface{ Scan(dest ...any) error }) (model.ReplayApplication, error) {
	var app model.ReplayApplication
	var applied, created string
	err := scanner.Scan(&app.ID, &app.CandidateID, &app.Agent, &app.Project, &app.PromptHash, &app.PromptExcerpt, &applied, &created)
	app.AppliedAt = parseTime(applied)
	app.CreatedAt = parseTime(created)
	return app, err
}

func scanExport(scanner interface{ Scan(dest ...any) error }) (model.ExportRecord, error) {
	var e model.ExportRecord
	var status, applied, rolled, created string
	err := scanner.Scan(&e.ID, &e.CandidateID, &e.Target, &e.Path, &status, &e.PlanJSON, &e.BackupPath, &e.BeforeHash, &e.AfterHash, &applied, &rolled, &created)
	e.Status = model.ExportStatus(status)
	e.AppliedAt = parseTime(applied)
	e.RolledBackAt = parseTime(rolled)
	e.CreatedAt = parseTime(created)
	return e, err
}

func formatTime(t time.Time) string {
	if t.IsZero() {
		return ""
	}
	return t.UTC().Format(time.RFC3339Nano)
}

func parseTime(s string) time.Time {
	if strings.TrimSpace(s) == "" {
		return time.Time{}
	}
	t, _ := time.Parse(time.RFC3339Nano, s)
	return t
}

func boolInt(v bool) int {
	if v {
		return 1
	}
	return 0
}

func replayApplicationID(candidateID string, t time.Time) string {
	base := strings.TrimPrefix(candidateID, "cand_")
	if base == "" {
		base = "unknown"
	}
	return fmt.Sprintf("rapp_%s_%d", base, t.UnixNano())
}

func canMarkAppliedOnce(state model.CandidateState) bool {
	switch state {
	case model.CandidatePending,
		model.CandidateProposalReady,
		model.CandidateProposed,
		model.CandidateAppliedOnce:
		return true
	default:
		return false
	}
}
