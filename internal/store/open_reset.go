package store

import (
	"database/sql"
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/sys/unix"
)

const CurrentSchemaGeneration = 7

var (
	ErrUnknownDatabase      = errors.New("unknown database target")
	ErrUnsafeDatabaseTarget = errors.New("unsafe database target")
)

var legacySchemaFingerprint = map[string][]string{
	"actions":               {"id", "turn_id", "tool_name", "command", "file_path", "excerpt"},
	"candidate_corrections": {"candidate_id", "correction_id"},
	"candidate_events":      {"candidate_id", "event_id"},
	"candidates": {"id", "fingerprint", "name", "description", "rule_text", "state", "event_count", "project_count",
		"source_count", "first_seen", "last_seen", "confidence", "version", "updated_at", "proposed_at", "snoozed_until",
		"skill_path", "semantic_key", "source_kind"},
	"corrections":         {"id", "session_id", "turn_id", "action_id", "hash", "normalized", "excerpt", "agent", "project", "created_at"},
	"events":              {"id", "hash", "normalized", "source", "agent", "project", "excerpt", "raw", "raw_stored", "created_at"},
	"exports":             {"id", "candidate_id", "target", "path", "status", "plan_json", "backup_path", "before_hash", "after_hash", "applied_at", "rolled_back_at", "created_at"},
	"replay_applications": {"id", "candidate_id", "agent", "project", "prompt_hash", "prompt_excerpt", "applied_at", "created_at"},
	"sessions":            {"id", "agent", "project", "source_path", "source_hash", "started_at", "updated_at"},
	"source_cursors":      {"source_path", "agent", "last_offset", "last_hash", "last_synced_at"},
	"turns":               {"id", "session_id", "turn_index", "role", "event_type", "created_at"},
}

func prepareDatabase(path string) (func(), bool, error) {
	lock, err := lockDatabaseTransition(path + ".open.lock")
	if err != nil {
		return nil, false, err
	}
	unlock := func() {
		_ = unix.Flock(int(lock.Fd()), unix.LOCK_UN)
		_ = lock.Close()
	}

	info, err := os.Lstat(path)
	if errors.Is(err, os.ErrNotExist) {
		for _, suffix := range []string{"-wal", "-shm"} {
			if _, sideErr := os.Lstat(path + suffix); sideErr == nil {
				unlock()
				return nil, false, fmt.Errorf("%w: orphan sqlite sidecar %s", ErrUnsafeDatabaseTarget, suffix)
			} else if !errors.Is(sideErr, os.ErrNotExist) {
				unlock()
				return nil, false, sideErr
			}
		}
		return unlock, false, nil
	}
	if err != nil {
		unlock()
		return nil, false, err
	}
	if !info.Mode().IsRegular() {
		unlock()
		return nil, false, fmt.Errorf("%w: database must be a regular file", ErrUnsafeDatabaseTarget)
	}
	for _, suffix := range []string{"-wal", "-shm"} {
		sideInfo, sideErr := os.Lstat(path + suffix)
		if errors.Is(sideErr, os.ErrNotExist) {
			continue
		}
		if sideErr != nil {
			unlock()
			return nil, false, sideErr
		}
		if !sideInfo.Mode().IsRegular() {
			unlock()
			return nil, false, fmt.Errorf("%w: sqlite sidecar %s is not a regular file", ErrUnsafeDatabaseTarget, suffix)
		}
	}

	kind, err := classifyDatabase(path)
	if err != nil {
		unlock()
		return nil, false, err
	}
	switch kind {
	case databaseCurrent:
		return unlock, false, nil
	case databaseLegacy:
		if err := removeVerifiedDatabase(path); err != nil {
			unlock()
			return nil, false, err
		}
		return unlock, true, nil
	default:
		unlock()
		return nil, false, ErrUnknownDatabase
	}
}

func lockDatabaseTransition(path string) (*os.File, error) {
	fd, err := unix.Open(path, unix.O_CREAT|unix.O_RDWR|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0o600)
	if err != nil {
		if errors.Is(err, unix.ELOOP) {
			return nil, fmt.Errorf("%w: transition lock is a symlink", ErrUnsafeDatabaseTarget)
		}
		return nil, err
	}
	f := os.NewFile(uintptr(fd), path)
	if err := unix.Flock(fd, unix.LOCK_EX); err != nil {
		_ = f.Close()
		return nil, err
	}
	return f, nil
}

type databaseKind int

const (
	databaseUnknown databaseKind = iota
	databaseLegacy
	databaseCurrent
)

func classifyDatabase(path string) (databaseKind, error) {
	u := url.URL{Scheme: "file", Path: path}
	db, err := sql.Open("sqlite3", u.String()+"?mode=ro&_query_only=1")
	if err != nil {
		return databaseUnknown, fmt.Errorf("%w: %v", ErrUnknownDatabase, err)
	}
	defer db.Close()
	var quickCheck string
	if err := db.QueryRow(`PRAGMA quick_check(1)`).Scan(&quickCheck); err != nil || quickCheck != "ok" {
		if err == nil {
			err = fmt.Errorf("quick_check returned %q", quickCheck)
		}
		return databaseUnknown, fmt.Errorf("%w: %v", ErrUnknownDatabase, err)
	}

	var generation int
	err = db.QueryRow(`SELECT generation FROM agbox_schema WHERE singleton = 1`).Scan(&generation)
	if err == nil {
		if generation == CurrentSchemaGeneration {
			return databaseCurrent, nil
		}
		return databaseUnknown, fmt.Errorf("%w: schema generation %d", ErrUnknownDatabase, generation)
	}
	if !strings.Contains(err.Error(), "no such table") && !errors.Is(err, sql.ErrNoRows) {
		return databaseUnknown, fmt.Errorf("%w: cannot inspect schema marker: %v", ErrUnknownDatabase, err)
	}

	rows, err := db.Query(`SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'`)
	if err != nil {
		return databaseUnknown, fmt.Errorf("%w: %v", ErrUnknownDatabase, err)
	}
	defer rows.Close()
	tables := make(map[string]bool)
	for rows.Next() {
		var name string
		if err := rows.Scan(&name); err != nil {
			return databaseUnknown, err
		}
		tables[name] = true
	}
	if err := rows.Err(); err != nil {
		return databaseUnknown, err
	}
	if len(tables) != len(legacySchemaFingerprint) {
		return databaseUnknown, fmt.Errorf("%w: legacy table set mismatch", ErrUnknownDatabase)
	}
	for table, expectedColumns := range legacySchemaFingerprint {
		if !tables[table] {
			return databaseUnknown, fmt.Errorf("%w: legacy fingerprint missing %s", ErrUnknownDatabase, table)
		}
		columns, err := tableColumns(db, table)
		if err != nil || !equalStrings(columns, expectedColumns) {
			return databaseUnknown, fmt.Errorf("%w: legacy fingerprint mismatch for %s", ErrUnknownDatabase, table)
		}
	}
	return databaseLegacy, nil
}

func tableColumns(db *sql.DB, table string) ([]string, error) {
	rows, err := db.Query(`PRAGMA table_info(` + table + `)`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var columns []string
	for rows.Next() {
		var cid, notNull, primaryKey int
		var name, columnType string
		var defaultValue any
		if err := rows.Scan(&cid, &name, &columnType, &notNull, &defaultValue, &primaryKey); err != nil {
			return nil, err
		}
		columns = append(columns, name)
	}
	return columns, rows.Err()
}

func equalStrings(left, right []string) bool {
	if len(left) != len(right) {
		return false
	}
	for i := range left {
		if left[i] != right[i] {
			return false
		}
	}
	return true
}

func removeVerifiedDatabase(path string) error {
	// Sidecars are removed first, leaving the verified main database available
	// for a deterministic retry if the process stops between removals.
	for _, target := range []string{path + "-wal", path + "-shm", path} {
		info, err := os.Lstat(target)
		if errors.Is(err, os.ErrNotExist) {
			continue
		}
		if err != nil {
			return err
		}
		if !info.Mode().IsRegular() {
			return fmt.Errorf("%w: %s is not a regular file", ErrUnsafeDatabaseTarget, filepath.Base(target))
		}
		if err := os.Remove(target); err != nil {
			return err
		}
	}
	return nil
}
