package scan

import (
	"path/filepath"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/store"
)

func TestRunAtAgesCorrectionOutOfScanAndCandidateConfidence(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	now := time.Date(2026, 7, 16, 12, 0, 0, 0, time.UTC)
	if err := s.SetHistoryWindow(7 * 24 * time.Hour); err != nil {
		t.Fatal(err)
	}
	if err := s.UpsertSession(model.Session{ID: "ses_age", Agent: "codex", Project: "repo", SourcePath: "/tmp/a.jsonl", SourceHash: "h", StartedAt: now, UpdatedAt: now}); err != nil {
		t.Fatal(err)
	}
	if err := s.InsertTurns([]model.Turn{{ID: "turn_age", SessionID: "ses_age", TurnIndex: 1, Role: "user", EventType: "message", CreatedAt: now}}); err != nil {
		t.Fatal(err)
	}
	if err := s.InsertActions([]model.Action{{ID: "act_age", TurnID: "turn_age", ToolName: "shell", Command: "npm install", Excerpt: "npm install"}}); err != nil {
		t.Fatal(err)
	}
	for _, item := range []struct {
		id string
		at time.Time
	}{
		{id: "cor_old", at: now.Add(-8 * 24 * time.Hour)},
		{id: "cor_current", at: now.Add(-7 * 24 * time.Hour)},
	} {
		if err := s.InsertCorrection(model.Correction{ID: item.id, SessionID: "ses_age", TurnID: "turn_age", ActionID: "act_age", Hash: item.id, Normalized: "use bun not npm", Excerpt: "Use bun not npm", Agent: "codex", Project: "repo", CreatedAt: item.at}); err != nil {
			t.Fatal(err)
		}
	}
	initial, err := RunAt(s, 2, now.Add(-7*24*time.Hour))
	if err != nil || len(initial.Candidates) != 1 {
		t.Fatalf("initial scan = %+v, %v", initial, err)
	}
	aged, err := RunAt(s, 2, now)
	if err != nil {
		t.Fatal(err)
	}
	if len(aged.Candidates) != 0 || aged.Scanned != 1 {
		t.Fatalf("aged scan = %+v", aged)
	}
	candidate, err := s.GetCandidate(initial.Candidates[0].ID)
	if err != nil {
		t.Fatal(err)
	}
	if candidate.State != model.CandidateInactive || candidate.EventCount != 1 || candidate.Confidence != "insufficient" {
		t.Fatalf("candidate after aging = %+v", candidate)
	}
}
