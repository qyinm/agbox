package scan

import (
	"path/filepath"
	"testing"

	"github.com/hippoom/agbox/internal/capture"
	"github.com/hippoom/agbox/internal/store"
)

func TestRunCreatesCandidateForRepeatedSignal(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	for i := 0; i < 2; i++ {
		if _, err := capture.Capture(s, "Use bun, not npm.", capture.Options{Project: "repo", Source: "manual"}); err != nil {
			t.Fatal(err)
		}
	}
	result, err := Run(s, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Candidates) != 1 {
		t.Fatalf("candidates = %d, want 1", len(result.Candidates))
	}
	c := result.Candidates[0]
	if c.EventCount != 2 {
		t.Fatalf("event count = %d, want 2", c.EventCount)
	}
	if c.Name != "use-bun-not-npm" {
		t.Fatalf("name = %q", c.Name)
	}
}
