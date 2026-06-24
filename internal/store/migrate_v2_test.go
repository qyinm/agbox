package store_test

import (
	"path/filepath"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/model"
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

func TestCandidateSourceKindDefaultsToPromptPattern(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	now := time.Now()
	c := model.Candidate{
		ID:          "cand_source_default",
		Fingerprint: "fp_source_default",
		Name:        "source-default",
		Description: "test",
		RuleText:    "test",
		State:       model.CandidatePending,
		EventCount:  2,
		FirstSeen:   now,
		LastSeen:    now,
		UpdatedAt:   now,
	}
	if err := s.UpsertCandidate(c, nil, nil); err != nil {
		t.Fatal(err)
	}
	got, err := s.GetCandidate(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.SourceKind != model.CandidateSourcePromptPattern {
		t.Fatalf("source kind = %q, want prompt_pattern", got.SourceKind)
	}
}
