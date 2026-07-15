package cursor_test

import (
	"errors"
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

func TestParseDeltaReturnsUnsupported(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "session.jsonl")
	if err := os.WriteFile(path, []byte(`{"type":"user"}`+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	adapter := cursor.New()
	src := session.Source{Agent: "cursor", Path: path, Project: "demo"}
	_, err := adapter.ParseDelta(src, session.Cursor{})
	if !errors.Is(err, session.ErrUnsupportedAdapter) {
		t.Fatalf("ParseDelta() error = %v, want ErrUnsupportedAdapter", err)
	}
}

func TestCursorIsVisibleButNotRunnable(t *testing.T) {
	adapter := cursor.New()
	if adapter.Runnable() {
		t.Fatal("Cursor must remain non-runnable until a native parser exists")
	}
	if got := len(adapter.RootSpecs()); got != 0 {
		t.Fatalf("Cursor roots = %d, want 0", got)
	}
}
