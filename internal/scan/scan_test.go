package scan

import (
	"path/filepath"
	"testing"

	"github.com/hippoom/agbox/internal/capture"
	"github.com/hippoom/agbox/internal/privacy"
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
	if c.Name != "package-manager-workflow" {
		t.Fatalf("name = %q", c.Name)
	}
}

func TestRunClustersSimilarPackageManagerCorrections(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	inputs := []string{
		"Use pnpm, not npm.",
		"Please use pnpm instead of npm.",
	}
	for _, input := range inputs {
		if _, err := capture.Capture(s, input, capture.Options{Project: "repo", Source: "manual"}); err != nil {
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
	if c.Name != "package-manager-workflow" {
		t.Fatalf("name = %q", c.Name)
	}
	if c.Fingerprint == privacy.HashSignal(privacy.NormalizeSignal(inputs[0])) {
		t.Fatalf("candidate used exact text hash, want semantic cluster fingerprint")
	}
}

func TestRunClustersWorkflowTaxonomySignals(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	inputs := []string{
		"Route changes require OpenAPI updates.",
		"When changing API routes, update the schema.",
	}
	for _, input := range inputs {
		if _, err := capture.Capture(s, input, capture.Options{Project: "repo", Source: "manual"}); err != nil {
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
	if result.Candidates[0].Name != "api-route-openapi-workflow" {
		t.Fatalf("name = %q", result.Candidates[0].Name)
	}
}
