package claude_test

import (
	"path/filepath"
	"runtime"
	"testing"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/session/claude"
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
	adapter := claude.New()
	src := session.Source{Agent: "claude", Path: testdataPath(t, "sample.jsonl"), Project: "demo"}
	result, err := adapter.ParseDelta(src, session.Cursor{})
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Corrections) != 1 {
		t.Fatalf("corrections = %d, want 1", len(result.Corrections))
	}
	if result.Corrections[0].Excerpt == "" {
		t.Fatal("expected redacted excerpt")
	}
}