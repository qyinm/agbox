package grok_test

import (
	"path/filepath"
	"testing"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/session/grok"
)

func TestParseDeltaFindsCorrections(t *testing.T) {
	adapter := grok.New()
	path := filepath.Join("testdata", "sample.jsonl")
	src := session.Source{Agent: "grok", Path: path, Project: "demo"}
	result, err := adapter.ParseDelta(src, session.Cursor{})
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Corrections) < 2 {
		t.Fatalf("corrections = %d, want >= 2", len(result.Corrections))
	}
}