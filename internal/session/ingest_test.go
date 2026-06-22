package session_test

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/session/claude"
	"github.com/hippoom/agbox/internal/store"
)

func claudeTestdataPath(t *testing.T, name string) string {
	t.Helper()
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	return filepath.Join(filepath.Dir(file), "claude", "testdata", name)
}

func TestIngestSourceStoresCorrectionsAndCursor(t *testing.T) {
	sample, err := os.ReadFile(claudeTestdataPath(t, "sample.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	srcPath := filepath.Join(dir, "sample.jsonl")
	if err := os.WriteFile(srcPath, sample, 0o600); err != nil {
		t.Fatal(err)
	}

	s, err := store.Open(filepath.Join(dir, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	adapter := claude.New()
	src := session.Source{Agent: "claude", Path: srcPath, Project: "demo"}
	if err := session.IngestSource(s, adapter, src); err != nil {
		t.Fatal(err)
	}

	count, err := s.CountCorrections()
	if err != nil {
		t.Fatal(err)
	}
	if count == 0 {
		t.Fatal("expected corrections to be ingested")
	}

	cursor, err := s.GetCursor(srcPath)
	if err != nil {
		t.Fatal(err)
	}
	if cursor.LastOffset == 0 {
		t.Fatal("expected cursor offset to be updated")
	}
	if cursor.LastHash == "" {
		t.Fatal("expected cursor hash to be set")
	}
	if cursor.Agent != "claude" {
		t.Fatalf("cursor agent = %q, want claude", cursor.Agent)
	}
}