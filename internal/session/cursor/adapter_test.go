package cursor_test

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/session/cursor"
)

func TestDiscoverSourcesDoesNotError(t *testing.T) {
	adapter := cursor.New()
	_, err := adapter.DiscoverSources()
	if err != nil {
		t.Fatalf("DiscoverSources() error = %v", err)
	}
}

func TestParseDeltaReturnsNoError(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "session.jsonl")
	if err := os.WriteFile(path, []byte(`{"type":"user"}`+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	adapter := cursor.New()
	src := session.Source{Agent: "cursor", Path: path, Project: "demo"}
	result, err := adapter.ParseDelta(src, session.Cursor{})
	if err != nil {
		t.Fatalf("ParseDelta() error = %v", err)
	}
	if len(result.Corrections) != 0 {
		t.Fatalf("corrections = %d, want 0", len(result.Corrections))
	}
}