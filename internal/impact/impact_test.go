package impact

import (
	"path/filepath"
	"testing"

	"github.com/hippoom/agbox/internal/capture"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/store"
)

func TestBuildDoesNotClaimReductionBeforeExport(t *testing.T) {
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
	result, err := scan.Run(s, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Candidates) != 1 {
		t.Fatalf("candidates = %d, want 1", len(result.Candidates))
	}

	meter, err := Build(s, result.Candidates[0].ID)
	if err != nil {
		t.Fatal(err)
	}
	if meter.Reduction != 0 {
		t.Fatalf("reduction = %d, want 0 before an applied export", meter.Reduction)
	}
	if meter.Confidence != "unmeasured" {
		t.Fatalf("confidence = %q, want unmeasured", meter.Confidence)
	}
}
