package store

import "database/sql"

func migrateV3(db *sql.DB) error {
	cols := []struct{ name, ddl string }{
		{"proposed_at", `ALTER TABLE candidates ADD COLUMN proposed_at TEXT NOT NULL DEFAULT ''`},
		{"snoozed_until", `ALTER TABLE candidates ADD COLUMN snoozed_until TEXT NOT NULL DEFAULT ''`},
		{"skill_path", `ALTER TABLE candidates ADD COLUMN skill_path TEXT NOT NULL DEFAULT ''`},
	}
	for _, col := range cols {
		if columnExists(db, "candidates", col.name) {
			continue
		}
		if _, err := db.Exec(col.ddl); err != nil {
			return err
		}
	}
	return nil
}

func columnExists(db *sql.DB, table, column string) bool {
	rows, err := db.Query(`PRAGMA table_info(` + table + `)`)
	if err != nil {
		return false
	}
	defer rows.Close()
	for rows.Next() {
		var cid int
		var name, ctype string
		var notnull, pk int
		var dflt any
		if err := rows.Scan(&cid, &name, &ctype, &notnull, &dflt, &pk); err != nil {
			return false
		}
		if name == column {
			return true
		}
	}
	return false
}