package store

import "database/sql"

func migrateV4(db *sql.DB) error {
	if columnExists(db, "candidates", "semantic_key") {
		return nil
	}
	_, err := db.Exec(`ALTER TABLE candidates ADD COLUMN semantic_key TEXT NOT NULL DEFAULT ''`)
	return err
}