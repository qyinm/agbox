package export

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/hippoom/agbox/internal/capture"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/store"
)

func TestApplyAndRollback(t *testing.T) {
	root := t.TempDir()
	s, err := store.Open(filepath.Join(root, "state", "agbox.db"))
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
	c := result.Candidates[0]
	if err := s.SetCandidateState(c.ID, model.CandidateApproved, "repo-package-manager"); err != nil {
		t.Fatal(err)
	}
	c, err = s.GetCandidate(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	rec, err := Apply(s, root, c, Options{Target: "agents-md"})
	if err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(filepath.Join(root, "AGENTS.md"))
	if err != nil {
		t.Fatal(err)
	}
	if len(data) == 0 {
		t.Fatal("AGENTS.md is empty after export")
	}
	if _, err := os.Stat(filepath.Join(root, ".agbox", "skill-pack.json")); err != nil {
		t.Fatal(err)
	}
	if _, err := Rollback(s, root, rec.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(root, "AGENTS.md")); !os.IsNotExist(err) {
		t.Fatalf("AGENTS.md still exists after rollback; err=%v", err)
	}
}

func TestBuildPlanRejectsParentTraversal(t *testing.T) {
	root := t.TempDir()
	c := model.Candidate{ID: "cand_test", Name: "test", Description: "test", RuleText: "test", State: model.CandidateApproved}
	if _, _, err := BuildPlan(root, c, Options{Target: "agents-md", Path: "../AGENTS.md"}); err == nil {
		t.Fatal("expected traversal path to be rejected")
	}
}
