package store

import "database/sql"

func migrateV6(db *sql.DB) error {
	stmts := []string{
		`CREATE TABLE IF NOT EXISTS replay_applications (
			id TEXT PRIMARY KEY,
			candidate_id TEXT NOT NULL,
			agent TEXT NOT NULL,
			project TEXT NOT NULL,
			prompt_hash TEXT NOT NULL,
			prompt_excerpt TEXT NOT NULL,
			applied_at TEXT NOT NULL,
			created_at TEXT NOT NULL,
			FOREIGN KEY(candidate_id) REFERENCES candidates(id) ON DELETE CASCADE
		)`,
		`CREATE INDEX IF NOT EXISTS idx_replay_applications_candidate ON replay_applications(candidate_id)`,
		`CREATE INDEX IF NOT EXISTS idx_replay_applications_applied ON replay_applications(applied_at)`,
	}
	for _, stmt := range stmts {
		if _, err := db.Exec(stmt); err != nil {
			return err
		}
	}
	return nil
}
