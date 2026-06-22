package grok_test

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/session/grok"
)

func testdataPath(t *testing.T, name string) string {
	t.Helper()
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	return filepath.Join(filepath.Dir(file), "testdata", name)
}

func TestParseDeltaDetectsCorrection(t *testing.T) {
	adapter := grok.New()
	src := session.Source{Agent: "grok", Path: testdataPath(t, "sample.jsonl"), Project: "agbox"}
	result, err := adapter.ParseDelta(src, session.Cursor{})
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Corrections) != 2 {
		t.Fatalf("corrections = %d, want 2", len(result.Corrections))
	}
	if result.Corrections[0].Excerpt == "" {
		t.Fatal("expected redacted excerpt")
	}
}

func TestProjectFromPathDecodesCWD(t *testing.T) {
	adapter := grok.New()
	home := t.TempDir()
	root := filepath.Join(home, ".grok", "sessions", "%2Ftmp%2Fagbox-demo", "session-1", "chat_history.jsonl")
	if err := os.MkdirAll(filepath.Dir(root), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(root, []byte(""), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HOME", home)
	sources, err := adapter.DiscoverSources()
	if err != nil {
		t.Fatal(err)
	}
	if len(sources) != 1 {
		t.Fatalf("sources = %d, want 1", len(sources))
	}
	if sources[0].Project != "agbox-demo" {
		t.Fatalf("project = %q, want agbox-demo", sources[0].Project)
	}
}