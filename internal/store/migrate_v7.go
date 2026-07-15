package store

import "database/sql"

func migrateV7(db *sql.DB) error {
	stmts := []string{
		`CREATE TABLE IF NOT EXISTS ingestion_sources (
			source_id TEXT NOT NULL,
			generation INTEGER NOT NULL,
			agent TEXT NOT NULL,
			source_ref TEXT NOT NULL,
			state TEXT NOT NULL,
			created_at TEXT NOT NULL,
			updated_at TEXT NOT NULL,
			PRIMARY KEY(source_id, generation)
		)`,
		`CREATE INDEX IF NOT EXISTS idx_ingestion_sources_state ON ingestion_sources(state, agent)`,
		`CREATE TABLE IF NOT EXISTS ingestion_checkpoints (
			source_id TEXT NOT NULL,
			generation INTEGER NOT NULL,
			committed_offset INTEGER NOT NULL DEFAULT 0 CHECK(committed_offset >= 0),
			parser_state_version INTEGER NOT NULL DEFAULT 0,
			parser_state BLOB NOT NULL DEFAULT X'',
			visibility_watermark INTEGER NOT NULL DEFAULT 0,
			updated_at TEXT NOT NULL,
			PRIMARY KEY(source_id, generation),
			FOREIGN KEY(source_id, generation) REFERENCES ingestion_sources(source_id, generation) ON DELETE CASCADE
		)`,
		`CREATE TABLE IF NOT EXISTS ingestion_work (
			source_id TEXT NOT NULL,
			generation INTEGER NOT NULL,
			work_class INTEGER NOT NULL,
			target_offset INTEGER NOT NULL CHECK(target_offset >= 0),
			state TEXT NOT NULL,
			retry_count INTEGER NOT NULL DEFAULT 0,
			failure_class TEXT NOT NULL DEFAULT '',
			active_fence INTEGER NOT NULL DEFAULT 0,
			created_at TEXT NOT NULL,
			updated_at TEXT NOT NULL,
			PRIMARY KEY(source_id, generation),
			FOREIGN KEY(source_id, generation) REFERENCES ingestion_sources(source_id, generation) ON DELETE CASCADE
		)`,
		`CREATE INDEX IF NOT EXISTS idx_ingestion_work_runnable ON ingestion_work(state, work_class, updated_at)`,
		`CREATE TABLE IF NOT EXISTS scheduler_lease (
			singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
			owner_id TEXT NOT NULL,
			fencing_token INTEGER NOT NULL,
			expires_at TEXT NOT NULL,
			heartbeat_at TEXT NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS ingestion_receipts (
			receipt_id TEXT PRIMARY KEY,
			source_id TEXT NOT NULL,
			generation INTEGER NOT NULL,
			target_offset INTEGER NOT NULL,
			status TEXT NOT NULL,
			failure_class TEXT NOT NULL DEFAULT '',
			created_at TEXT NOT NULL,
			completed_at TEXT NOT NULL DEFAULT '',
			FOREIGN KEY(source_id, generation) REFERENCES ingestion_sources(source_id, generation) ON DELETE CASCADE
		)`,
		`CREATE INDEX IF NOT EXISTS idx_ingestion_receipts_source ON ingestion_receipts(source_id, generation, created_at)`,
		`CREATE TABLE IF NOT EXISTS consumer_visibility (
			singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
			watermark INTEGER NOT NULL,
			committed_at TEXT NOT NULL
		)`,
		`INSERT INTO consumer_visibility(singleton, watermark, committed_at)
			VALUES (1, 0, '') ON CONFLICT(singleton) DO NOTHING`,
		`CREATE TABLE IF NOT EXISTS ingestion_policy (
			singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
			history_window_seconds INTEGER NOT NULL CHECK(history_window_seconds > 0),
			updated_at TEXT NOT NULL
		)`,
		`INSERT INTO ingestion_policy(singleton, history_window_seconds, updated_at)
			VALUES (1, 7776000, '') ON CONFLICT(singleton) DO NOTHING`,
	}
	for _, stmt := range stmts {
		if _, err := db.Exec(stmt); err != nil {
			return err
		}
	}
	return nil
}
