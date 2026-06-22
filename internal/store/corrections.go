package store

import (
	"database/sql"
	"errors"
	"time"

	"github.com/hippoom/agbox/internal/model"
)

type CursorRow struct {
	SourcePath   string
	Agent        string
	LastOffset   int64
	LastHash     string
	LastSyncedAt time.Time
}

func (s *Store) UpsertSession(sess model.Session) error {
	_, err := s.db.Exec(`INSERT INTO sessions
		(id, agent, project, source_path, source_hash, started_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(id) DO UPDATE SET
			source_hash=excluded.source_hash,
			updated_at=excluded.updated_at`,
		sess.ID, sess.Agent, sess.Project, sess.SourcePath, sess.SourceHash,
		formatTime(sess.StartedAt), formatTime(sess.UpdatedAt))
	return err
}

func (s *Store) InsertTurns(turns []model.Turn) error {
	if len(turns) == 0 {
		return nil
	}
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	for _, t := range turns {
		if _, err := tx.Exec(`INSERT OR IGNORE INTO turns
			(id, session_id, turn_index, role, event_type, created_at)
			VALUES (?, ?, ?, ?, ?, ?)`,
			t.ID, t.SessionID, t.TurnIndex, t.Role, t.EventType, formatTime(t.CreatedAt)); err != nil {
			return err
		}
	}
	return tx.Commit()
}

func (s *Store) InsertActions(actions []model.Action) error {
	if len(actions) == 0 {
		return nil
	}
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	for _, a := range actions {
		if _, err := tx.Exec(`INSERT OR IGNORE INTO actions
			(id, turn_id, tool_name, command, file_path, excerpt)
			VALUES (?, ?, ?, ?, ?, ?)`,
			a.ID, a.TurnID, a.ToolName, a.Command, a.FilePath, a.Excerpt); err != nil {
			return err
		}
	}
	return tx.Commit()
}

func (s *Store) InsertCorrection(c model.Correction) error {
	_, err := s.db.Exec(`INSERT OR IGNORE INTO corrections
		(id, session_id, turn_id, action_id, hash, normalized, excerpt, agent, project, created_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		c.ID, c.SessionID, c.TurnID, c.ActionID, c.Hash, c.Normalized, c.Excerpt,
		c.Agent, c.Project, formatTime(c.CreatedAt))
	return err
}

func (s *Store) GetCursor(sourcePath string) (CursorRow, error) {
	row := s.db.QueryRow(`SELECT source_path, agent, last_offset, last_hash, last_synced_at
		FROM source_cursors WHERE source_path = ?`, sourcePath)
	var c CursorRow
	var synced string
	err := row.Scan(&c.SourcePath, &c.Agent, &c.LastOffset, &c.LastHash, &synced)
	if errors.Is(err, sql.ErrNoRows) {
		return CursorRow{SourcePath: sourcePath}, nil
	}
	if err != nil {
		return CursorRow{}, err
	}
	c.LastSyncedAt = parseTime(synced)
	return c, nil
}

func (s *Store) UpsertCursor(row CursorRow) error {
	_, err := s.db.Exec(`INSERT INTO source_cursors
		(source_path, agent, last_offset, last_hash, last_synced_at)
		VALUES (?, ?, ?, ?, ?)
		ON CONFLICT(source_path) DO UPDATE SET
			agent=excluded.agent,
			last_offset=excluded.last_offset,
			last_hash=excluded.last_hash,
			last_synced_at=excluded.last_synced_at`,
		row.SourcePath, row.Agent, row.LastOffset, row.LastHash, formatTime(row.LastSyncedAt))
	return err
}

func (s *Store) ListCorrections() ([]model.Correction, error) {
	rows, err := s.db.Query(`SELECT id, session_id, turn_id, action_id, hash, normalized, excerpt, agent, project, created_at
		FROM corrections ORDER BY created_at ASC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []model.Correction
	for rows.Next() {
		c, err := scanCorrection(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, c)
	}
	return out, rows.Err()
}

func (s *Store) CorrectionsForCandidate(candidateID string) ([]model.Correction, error) {
	rows, err := s.db.Query(`SELECT c.id, c.session_id, c.turn_id, c.action_id, c.hash, c.normalized, c.excerpt, c.agent, c.project, c.created_at
		FROM corrections c
		JOIN candidate_corrections cc ON cc.correction_id = c.id
		WHERE cc.candidate_id = ?
		ORDER BY c.created_at ASC`, candidateID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []model.Correction
	for rows.Next() {
		c, err := scanCorrection(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, c)
	}
	return out, rows.Err()
}

func (s *Store) CountCorrections() (int, error) {
	var n int
	err := s.db.QueryRow(`SELECT COUNT(*) FROM corrections`).Scan(&n)
	return n, err
}

func (s *Store) ListCursors() ([]CursorRow, error) {
	rows, err := s.db.Query(`SELECT source_path, agent, last_offset, last_hash, last_synced_at
		FROM source_cursors ORDER BY last_synced_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []CursorRow
	for rows.Next() {
		var c CursorRow
		var synced string
		if err := rows.Scan(&c.SourcePath, &c.Agent, &c.LastOffset, &c.LastHash, &synced); err != nil {
			return nil, err
		}
		c.LastSyncedAt = parseTime(synced)
		out = append(out, c)
	}
	return out, rows.Err()
}

func (s *Store) LatestCursorSync() (time.Time, error) {
	var synced string
	err := s.db.QueryRow(`SELECT MAX(last_synced_at) FROM source_cursors`).Scan(&synced)
	if err != nil {
		return time.Time{}, err
	}
	return parseTime(synced), nil
}

func (s *Store) GetTurn(id string) (model.Turn, error) {
	row := s.db.QueryRow(`SELECT id, session_id, turn_index, role, event_type, created_at FROM turns WHERE id = ?`, id)
	var t model.Turn
	var created string
	err := row.Scan(&t.ID, &t.SessionID, &t.TurnIndex, &t.Role, &t.EventType, &created)
	if errors.Is(err, sql.ErrNoRows) {
		return model.Turn{}, errors.New("turn not found")
	}
	if err != nil {
		return model.Turn{}, err
	}
	t.CreatedAt = parseTime(created)
	return t, nil
}

func (s *Store) GetAction(id string) (model.Action, error) {
	row := s.db.QueryRow(`SELECT id, turn_id, tool_name, command, file_path, excerpt FROM actions WHERE id = ?`, id)
	var a model.Action
	err := row.Scan(&a.ID, &a.TurnID, &a.ToolName, &a.Command, &a.FilePath, &a.Excerpt)
	if errors.Is(err, sql.ErrNoRows) {
		return model.Action{}, errors.New("action not found")
	}
	return a, err
}

func scanCorrection(scanner interface{ Scan(dest ...any) error }) (model.Correction, error) {
	var c model.Correction
	var created string
	err := scanner.Scan(&c.ID, &c.SessionID, &c.TurnID, &c.ActionID, &c.Hash, &c.Normalized, &c.Excerpt,
		&c.Agent, &c.Project, &created)
	c.CreatedAt = parseTime(created)
	return c, err
}