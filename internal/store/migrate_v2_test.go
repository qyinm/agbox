package store_test

import (
	"path/filepath"
	"testing"

	"github.com/hippoom/agbox/internal/store"
)

func TestMigrateV2CreatesCorrectionTables(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	for _, table := range []string{"sessions", "turns", "actions", "corrections", "source_cursors", "candidate_corrections"} {
		if !s.TableExists(table) {
			t.Fatalf("table %q not created", table)
		}
	}
}