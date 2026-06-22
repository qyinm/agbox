package scan

import (
	"fmt"
	"path/filepath"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/capture"
	"github.com/hippoom/agbox/internal/model"
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

func TestRunClustersCorrectionsWithSameAction(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	normalized := privacy.NormalizeSignal("Use bun, not npm.")
	actionCmd := "npm install"
	now := time.Now()

	sess := model.Session{
		ID:         "sess_1",
		Agent:      "claude",
		Project:    "repo",
		SourcePath: "/tmp/session.jsonl",
		SourceHash: "abc",
		StartedAt:  now,
		UpdatedAt:  now,
	}
	if err := s.UpsertSession(sess); err != nil {
		t.Fatal(err)
	}

	turnAgent := model.Turn{
		ID:        "turn_agent",
		SessionID: sess.ID,
		TurnIndex: 1,
		Role:      "agent",
		EventType: "tool",
		CreatedAt: now,
	}
	turnUser := model.Turn{
		ID:        "turn_user",
		SessionID: sess.ID,
		TurnIndex: 2,
		Role:      "user",
		EventType: "message",
		CreatedAt: now.Add(time.Minute),
	}
	if err := s.InsertTurns([]model.Turn{turnAgent, turnUser}); err != nil {
		t.Fatal(err)
	}

	action := model.Action{
		ID:       "act_1",
		TurnID:   turnAgent.ID,
		ToolName: "run_terminal_cmd",
		Command:  actionCmd,
		FilePath: "",
		Excerpt:  actionCmd,
	}
	if err := s.InsertActions([]model.Action{action}); err != nil {
		t.Fatal(err)
	}

	for i := 0; i < 3; i++ {
		correction := model.Correction{
			ID:         fmt.Sprintf("cor_%d", i),
			SessionID:  sess.ID,
			TurnID:     turnUser.ID,
			ActionID:   action.ID,
			Hash:       privacy.HashSignal(normalized),
			Normalized: normalized,
			Excerpt:    "Use bun, not npm.",
			Agent:      "claude",
			Project:    "repo",
			CreatedAt:  now.Add(time.Duration(i+1) * time.Minute),
		}
		if err := s.InsertCorrection(correction); err != nil {
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
	if result.Candidates[0].EventCount < 2 {
		t.Fatalf("event count = %d, want >= 2", result.Candidates[0].EventCount)
	}
}
