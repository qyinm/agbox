package store

import (
	"database/sql"
	"errors"
	"fmt"
	"time"

	"github.com/hippoom/agbox/internal/history"
	"github.com/hippoom/agbox/internal/model"
)

const DefaultHistoryWindow = history.DefaultWindow

type AgingResult struct {
	ExpiredCorrections    int
	DeactivatedCandidates int
}

type ConsumerCompleteness string

const (
	ConsumerComplete    ConsumerCompleteness = "complete"
	ConsumerIncomplete  ConsumerCompleteness = "incomplete"
	ConsumerQuarantined ConsumerCompleteness = "quarantined"
)

type ConsumerState struct {
	Completeness   ConsumerCompleteness `json:"completeness"`
	Pending        int                  `json:"pending"`
	LivePending    int                  `json:"live_pending"`
	CatchupPending int                  `json:"catchup_pending"`
	Quarantined    int                  `json:"quarantined"`
}

func (s *Store) HistoryWindow() (time.Duration, error) {
	var seconds int64
	if err := s.db.QueryRow(`SELECT history_window_seconds FROM ingestion_policy WHERE singleton = 1`).Scan(&seconds); err != nil {
		return 0, err
	}
	if seconds <= 0 {
		return 0, errors.New("invalid persisted history window")
	}
	return time.Duration(seconds) * time.Second, nil
}

func (s *Store) SetHistoryWindow(window time.Duration) error {
	if window <= 0 || window < time.Second || window%time.Second != 0 {
		return fmt.Errorf("history window must be a positive whole number of seconds")
	}
	_, err := s.db.Exec(`UPDATE ingestion_policy SET history_window_seconds = ?, updated_at = ? WHERE singleton = 1`, int64(window/time.Second), formatTime(time.Now()))
	return err
}

func (s *Store) ActiveCorrectionCutoff(now time.Time) (time.Time, error) {
	window, err := s.HistoryWindow()
	if err != nil {
		return time.Time{}, err
	}
	return history.Cutoff(now, window), nil
}

func (s *Store) ListActiveCorrectionsAt(now time.Time) ([]model.Correction, error) {
	cutoff, err := s.ActiveCorrectionCutoff(now)
	if err != nil {
		return nil, err
	}
	rows, err := s.db.Query(`SELECT id, session_id, turn_id, action_id, hash, normalized, excerpt, agent, project, created_at
		FROM corrections WHERE created_at >= ? ORDER BY created_at ASC`, formatTime(cutoff))
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanCorrections(rows)
}

func (s *Store) CorrectionsForCandidateAt(candidateID string, now time.Time) ([]model.Correction, error) {
	cutoff, err := s.ActiveCorrectionCutoff(now)
	if err != nil {
		return nil, err
	}
	rows, err := s.db.Query(`SELECT c.id, c.session_id, c.turn_id, c.action_id, c.hash, c.normalized, c.excerpt, c.agent, c.project, c.created_at
		FROM corrections c
		JOIN candidate_corrections cc ON cc.correction_id = c.id
		WHERE cc.candidate_id = ? AND c.created_at >= ?
		ORDER BY c.created_at ASC`, candidateID, formatTime(cutoff))
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanCorrections(rows)
}

func scanCorrections(rows *sql.Rows) ([]model.Correction, error) {
	var out []model.Correction
	for rows.Next() {
		correction, err := scanCorrection(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, correction)
	}
	return out, rows.Err()
}

func (s *Store) ActiveCorrectionCount(candidateID string, now time.Time) (int, error) {
	cutoff, err := s.ActiveCorrectionCutoff(now)
	if err != nil {
		return 0, err
	}
	var count int
	err = s.db.QueryRow(`SELECT COUNT(*) FROM candidate_corrections cc
		JOIN corrections c ON c.id = cc.correction_id
		WHERE cc.candidate_id = ? AND c.created_at >= ?`, candidateID, formatTime(cutoff)).Scan(&count)
	return count, err
}

// ApplyEvidenceAging atomically removes expired evidence from active links,
// redacts its retained tombstone, and deactivates candidates below threshold.
func (s *Store) ApplyEvidenceAging(now time.Time, minEvidence int) (AgingResult, error) {
	if minEvidence <= 0 {
		minEvidence = 2
	}
	cutoff, err := s.ActiveCorrectionCutoff(now)
	if err != nil {
		return AgingResult{}, err
	}
	tx, err := s.db.Begin()
	if err != nil {
		return AgingResult{}, err
	}
	defer tx.Rollback()

	rows, err := tx.Query(`SELECT DISTINCT cc.candidate_id FROM candidate_corrections cc
		JOIN corrections c ON c.id = cc.correction_id WHERE c.created_at < ?`, formatTime(cutoff))
	if err != nil {
		return AgingResult{}, err
	}
	var affected []string
	for rows.Next() {
		var id string
		if err := rows.Scan(&id); err != nil {
			rows.Close()
			return AgingResult{}, err
		}
		affected = append(affected, id)
	}
	if err := rows.Close(); err != nil {
		return AgingResult{}, err
	}

	var result AgingResult
	if err := tx.QueryRow(`SELECT COUNT(*) FROM corrections WHERE created_at < ? AND (normalized <> '' OR excerpt <> '')`, formatTime(cutoff)).Scan(&result.ExpiredCorrections); err != nil {
		return AgingResult{}, err
	}
	if _, err := tx.Exec(`DELETE FROM candidate_corrections WHERE correction_id IN
		(SELECT id FROM corrections WHERE created_at < ?)`, formatTime(cutoff)); err != nil {
		return AgingResult{}, err
	}
	if _, err := tx.Exec(`UPDATE actions SET command='', file_path='', excerpt=''
		WHERE id IN (SELECT action_id FROM corrections WHERE created_at < ?)
		AND id NOT IN (SELECT action_id FROM corrections WHERE created_at >= ?)`, formatTime(cutoff), formatTime(cutoff)); err != nil {
		return AgingResult{}, err
	}
	if _, err := tx.Exec(`UPDATE corrections SET normalized='', excerpt='' WHERE created_at < ?`, formatTime(cutoff)); err != nil {
		return AgingResult{}, err
	}
	for _, candidateID := range affected {
		var count, projects, sources int
		var first, last sql.NullString
		if err := tx.QueryRow(`SELECT COUNT(*), COUNT(DISTINCT c.project), COUNT(DISTINCT c.agent), MIN(c.created_at), MAX(c.created_at)
			FROM candidate_corrections cc JOIN corrections c ON c.id=cc.correction_id
			WHERE cc.candidate_id=?`, candidateID).Scan(&count, &projects, &sources, &first, &last); err != nil {
			return AgingResult{}, err
		}
		confidence := correctionConfidence(count, projects)
		stateExpr := `state`
		clearRecommendation := ""
		args := []any{count, projects, sources, confidence, formatTime(now)}
		if count < minEvidence {
			stateExpr = `?`
			clearRecommendation = `, description='', rule_text='', semantic_key=''`
			args = append(args, model.CandidateInactive)
			result.DeactivatedCandidates++
		}
		args = append(args, candidateID)
		query := `UPDATE candidates SET event_count=?, project_count=?, source_count=?, confidence=?, updated_at=?, state=` + stateExpr + clearRecommendation
		if first.Valid && last.Valid {
			query += `, first_seen=?, last_seen=?`
			args = append(args[:len(args)-1], first.String, last.String, candidateID)
		}
		query += ` WHERE id=?`
		if _, err := tx.Exec(query, args...); err != nil {
			return AgingResult{}, err
		}
	}
	if err := tx.Commit(); err != nil {
		return AgingResult{}, err
	}
	return result, nil
}

func correctionConfidence(count, projects int) string {
	switch {
	case count < 2:
		return "insufficient"
	case count >= 5 || projects >= 2:
		return "high"
	case count >= 3:
		return "medium"
	default:
		return "low"
	}
}

func (s *Store) ConsumerState() (ConsumerState, error) {
	var state ConsumerState
	if err := s.db.QueryRow(`SELECT
		COALESCE(SUM(CASE WHEN state IN ('pending','running','waiting_append') THEN 1 ELSE 0 END), 0),
		COALESCE(SUM(CASE WHEN work_class=? AND state IN ('pending','running','waiting_append') THEN 1 ELSE 0 END), 0),
		COALESCE(SUM(CASE WHEN work_class IN (?,?) AND state IN ('pending','running','waiting_append') THEN 1 ELSE 0 END), 0),
		COALESCE(SUM(CASE WHEN state='quarantined' THEN 1 ELSE 0 END), 0)
		FROM ingestion_work`, WorkLive, WorkActiveCatchup, WorkArchive).Scan(&state.Pending, &state.LivePending, &state.CatchupPending, &state.Quarantined); err != nil {
		return ConsumerState{}, err
	}
	switch {
	case state.Quarantined > 0:
		state.Completeness = ConsumerQuarantined
	case state.Pending > 0:
		state.Completeness = ConsumerIncomplete
	default:
		state.Completeness = ConsumerComplete
	}
	return state, nil
}
