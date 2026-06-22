package store

import "database/sql"

func migrateV2(db *sql.DB) error {
	stmts := []string{
		`CREATE TABLE IF NOT EXISTS sessions (
			id TEXT PRIMARY KEY,
			agent TEXT NOT NULL,
			project TEXT NOT NULL,
			source_path TEXT NOT NULL,
			source_hash TEXT NOT NULL,
			started_at TEXT NOT NULL,
			updated_at TEXT NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS turns (
			id TEXT PRIMARY KEY,
			session_id TEXT NOT NULL,
			turn_index INTEGER NOT NULL,
			role TEXT NOT NULL,
			event_type TEXT NOT NULL,
			created_at TEXT NOT NULL,
			FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
		)`,
		`CREATE TABLE IF NOT EXISTS actions (
			id TEXT PRIMARY KEY,
			turn_id TEXT NOT NULL,
			tool_name TEXT NOT NULL,
			command TEXT NOT NULL,
			file_path TEXT NOT NULL,
			excerpt TEXT NOT NULL,
			FOREIGN KEY(turn_id) REFERENCES turns(id) ON DELETE CASCADE
		)`,
		`CREATE TABLE IF NOT EXISTS corrections (
			id TEXT PRIMARY KEY,
			session_id TEXT NOT NULL,
			turn_id TEXT NOT NULL,
			action_id TEXT NOT NULL,
			hash TEXT NOT NULL,
			normalized TEXT NOT NULL,
			excerpt TEXT NOT NULL,
			agent TEXT NOT NULL,
			project TEXT NOT NULL,
			created_at TEXT NOT NULL,
			FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE,
			FOREIGN KEY(turn_id) REFERENCES turns(id) ON DELETE CASCADE,
			FOREIGN KEY(action_id) REFERENCES actions(id) ON DELETE CASCADE
		)`,
		`CREATE TABLE IF NOT EXISTS source_cursors (
			source_path TEXT PRIMARY KEY,
			agent TEXT NOT NULL,
			last_offset INTEGER NOT NULL,
			last_hash TEXT NOT NULL,
			last_synced_at TEXT NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS candidate_corrections (
			candidate_id TEXT NOT NULL,
			correction_id TEXT NOT NULL,
			PRIMARY KEY(candidate_id, correction_id),
			FOREIGN KEY(candidate_id) REFERENCES candidates(id) ON DELETE CASCADE,
			FOREIGN KEY(correction_id) REFERENCES corrections(id) ON DELETE CASCADE
		)`,
	}
	for _, stmt := range stmts {
		if _, err := db.Exec(stmt); err != nil {
			return err
		}
	}
	return nil
}