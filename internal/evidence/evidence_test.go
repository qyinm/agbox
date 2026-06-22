package evidence_test

import (
	"fmt"
	"path/filepath"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/evidence"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/store"
)

func TestBuildReturnsOccurrencesFromCorrections(t *testing.T) {
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
		Excerpt:  actionCmd,
	}
	if err := s.InsertActions([]model.Action{action}); err != nil {
		t.Fatal(err)
	}

	for i := 0; i < 2; i++ {
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

	result, err := scan.Run(s, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Candidates) != 1 {
		t.Fatalf("candidates = %d, want 1", len(result.Candidates))
	}

	card, err := evidence.Build(s, result.Candidates[0].ID)
	if err != nil {
		t.Fatal(err)
	}
	if len(card.Occurrences) < 1 {
		t.Fatalf("occurrences = %d, want >= 1", len(card.Occurrences))
	}

	occ := card.Occurrences[0]
	summary := occ.SummaryLine()
	if summary == "" {
		t.Fatal("SummaryLine() returned empty string")
	}
	if occ.AgentAction == "" || occ.UserCorrection == "" {
		t.Fatalf("occurrence missing action or correction: %+v", occ)
	}
	if len(occ.DrillDown) < 2 {
		t.Fatalf("DrillDown len = %d, want >= 2", len(occ.DrillDown))
	}
	if occ.DrillDown[0].Role != "agent" {
		t.Fatalf("first drill step role = %q, want agent", occ.DrillDown[0].Role)
	}
	if occ.DrillDown[1].Role != "user" {
		t.Fatalf("second drill step role = %q, want user", occ.DrillDown[1].Role)
	}
}

func TestBuildFallsBackToEventExcerpts(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	for i := 0; i < 2; i++ {
		e := model.Event{
			ID:         fmt.Sprintf("evt_%d", i),
			Hash:       privacy.HashSignal("use bun not npm"),
			Normalized: "use bun not npm",
			Source:     "manual",
			Agent:      "manual",
			Project:    "repo",
			Excerpt:    "Use bun, not npm.",
			CreatedAt:  time.Now(),
		}
		if err := s.InsertEvent(e); err != nil {
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

	card, err := evidence.Build(s, result.Candidates[0].ID)
	if err != nil {
		t.Fatal(err)
	}
	if len(card.Occurrences) != 0 {
		t.Fatalf("occurrences = %d, want 0 for event-only candidates", len(card.Occurrences))
	}
	if len(card.Excerpts) == 0 {
		t.Fatal("expected excerpts from legacy events")
	}
}