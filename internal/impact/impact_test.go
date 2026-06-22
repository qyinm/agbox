package impact

import (
	"path/filepath"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/capture"
	"github.com/hippoom/agbox/internal/model"
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

func TestBuildMeasuresReductionAfterAcceptance(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	acceptedAt := time.Now().Add(-48 * time.Hour)
	beforeAccept := acceptedAt.Add(-24 * time.Hour)
	afterAccept := acceptedAt.Add(12 * time.Hour)
	now := time.Now()

	if err := s.UpsertSession(model.Session{
		ID: "ses_1", Agent: "grok", Project: "agbox", SourcePath: "/tmp/s.jsonl",
		SourceHash: "h", StartedAt: now, UpdatedAt: now,
	}); err != nil {
		t.Fatal(err)
	}
	if err := s.InsertTurns([]model.Turn{
		{ID: "turn_1", SessionID: "ses_1", TurnIndex: 1, Role: "agent", EventType: "tool", CreatedAt: now},
	}); err != nil {
		t.Fatal(err)
	}
	if err := s.InsertActions([]model.Action{
		{ID: "act_1", TurnID: "turn_1", ToolName: "Shell", Command: "npm install", Excerpt: "npm install"},
	}); err != nil {
		t.Fatal(err)
	}

	for _, item := range []struct {
		id, at string
	}{
		{"cor_before1", beforeAccept.Format(time.RFC3339)},
		{"cor_before2", beforeAccept.Add(time.Hour).Format(time.RFC3339)},
		{"cor_before3", beforeAccept.Add(2 * time.Hour).Format(time.RFC3339)},
		{"cor_after1", afterAccept.Format(time.RFC3339)},
	} {
		if err := s.InsertCorrection(model.Correction{
			ID: item.id, SessionID: "ses_1", TurnID: "turn_1", ActionID: "act_1",
			Hash: "h_" + item.id, Normalized: "use bun not npm", Excerpt: "use bun not npm",
			Agent: "grok", Project: "agbox", CreatedAt: parseTestTime(t, item.at),
		}); err != nil {
			t.Fatal(err)
		}
	}
	c := model.Candidate{
		ID:          "cand_accept123",
		Fingerprint: "fp_accept12345",
		Name:        "package-manager-workflow",
		Description: "use bun not npm",
		RuleText:    "use bun not npm",
		State:       model.CandidateAccepted,
		EventCount:  4,
		Confidence:  "high",
		ProposedAt:  acceptedAt,
		FirstSeen:   beforeAccept,
		LastSeen:    afterAccept,
		UpdatedAt:   afterAccept,
	}
	if err := s.UpsertCandidate(c, nil, []string{"cor_before1", "cor_before2", "cor_before3", "cor_after1"}); err != nil {
		t.Fatal(err)
	}
	if err := s.UpdateCandidateMeta(c.ID, store.CandidateMetaUpdate{
		State:      model.CandidateAccepted,
		ProposedAt: &acceptedAt,
	}); err != nil {
		t.Fatal(err)
	}

	meter, err := Build(s, c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if meter.Before != 3 {
		t.Fatalf("before = %d, want 3", meter.Before)
	}
	if meter.After != 1 {
		t.Fatalf("after = %d, want 1", meter.After)
	}
	if meter.Reduction != 2 {
		t.Fatalf("reduction = %d, want 2", meter.Reduction)
	}
	if meter.Confidence != "medium" {
		t.Fatalf("confidence = %q, want medium", meter.Confidence)
	}
}

func parseTestTime(t *testing.T, value string) time.Time {
	t.Helper()
	parsed, err := time.Parse(time.RFC3339, value)
	if err != nil {
		t.Fatal(err)
	}
	return parsed
}
