package store

import "database/sql"

func migrateV5(db *sql.DB) error {
	if !columnExists(db, "candidates", "source_kind") {
		if _, err := db.Exec(`ALTER TABLE candidates ADD COLUMN source_kind TEXT NOT NULL DEFAULT 'prompt_pattern'`); err != nil {
			return err
		}
	}
	_, err := db.Exec(`UPDATE candidates
		SET source_kind = 'correction'
		WHERE id IN (SELECT DISTINCT candidate_id FROM candidate_corrections)`)
	return err
}
