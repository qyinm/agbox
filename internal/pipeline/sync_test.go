package pipeline_test

import (
	"errors"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/pipeline"
	"github.com/hippoom/agbox/internal/privacy"
	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
)

type failingAdapter struct{}

func (failingAdapter) Agent() string {
	return "zz_test_failure"
}

func (failingAdapter) DiscoverSources() ([]session.Source, error) {
	return nil, errors.New("test ingest failure")
}

func (failingAdapter) ParseDelta(session.Source, session.Cursor) (session.ParseResult, error) {
	return session.ParseResult{}, nil
}

func init() {
	session.Register(failingAdapter{})
}

func TestSyncAllPreservesFailFastContract(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	seedCorrectionPair(t, s)

	_, err = pipeline.SyncAll(s)
	if err == nil || !strings.Contains(err.Error(), "test ingest failure") {
		t.Fatalf("SyncAll error = %v, want test ingest failure", err)
	}

	candidates, err := s.ListCandidatesByState(model.CandidateProposalReady)
	if err != nil {
		t.Fatal(err)
	}
	if len(candidates) != 0 {
		t.Fatalf("proposal_ready candidates = %d, want 0", len(candidates))
	}
}

func TestSyncBestEffortScansAndPromotesAfterIngestFailure(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	seedCorrectionPair(t, s)

	result, err := pipeline.SyncBestEffort(s)
	if err != nil {
		t.Fatalf("SyncBestEffort fatal error = %v", err)
	}
	if result.Warning == nil || !strings.Contains(result.Warning.Error(), "test ingest failure") {
		t.Fatalf("SyncBestEffort warning = %v, want test ingest failure", result.Warning)
	}

	candidates, err := s.ListCandidatesByState(model.CandidateProposalReady)
	if err != nil {
		t.Fatal(err)
	}
	if len(candidates) != 1 {
		t.Fatalf("proposal_ready candidates = %d, want 1", len(candidates))
	}
	if candidates[0].SemanticKey != "package-manager:bun-over-npm" {
		t.Fatalf("candidate semantic key = %q, want package-manager:bun-over-npm", candidates[0].SemanticKey)
	}
}

func TestSyncBestEffortIfStaleStillScansAndPromotes(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	seedCorrectionPair(t, s)
	if err := s.UpsertCursor(store.CursorRow{
		SourcePath:   filepath.Join(t.TempDir(), "session.jsonl"),
		Agent:        "claude",
		LastSyncedAt: time.Now(),
	}); err != nil {
		t.Fatal(err)
	}

	result, err := pipeline.SyncBestEffortIfStale(s)
	if err != nil {
		t.Fatalf("SyncBestEffortIfStale error = %v", err)
	}
	if !result.IngestSkipped {
		t.Fatal("SyncBestEffortIfStale did not report skipped ingest")
	}

	candidates, err := s.ListCandidatesByState(model.CandidateProposalReady)
	if err != nil {
		t.Fatal(err)
	}
	if len(candidates) != 1 {
		t.Fatalf("proposal_ready candidates = %d, want 1", len(candidates))
	}
}

func seedCorrectionPair(t *testing.T, s *store.Store) {
	t.Helper()
	now := time.Now()
	sess := model.Session{
		ID:         "sess_pipeline",
		Agent:      "claude",
		Project:    "repo",
		SourcePath: filepath.Join(t.TempDir(), "session.jsonl"),
		SourceHash: "hash",
		StartedAt:  now,
		UpdatedAt:  now,
	}
	if err := s.UpsertSession(sess); err != nil {
		t.Fatal(err)
	}
	agentTurn := model.Turn{ID: "turn_pipeline_agent", SessionID: sess.ID, TurnIndex: 1, Role: "agent", EventType: "tool", CreatedAt: now}
	userTurn := model.Turn{ID: "turn_pipeline_user", SessionID: sess.ID, TurnIndex: 2, Role: "user", EventType: "message", CreatedAt: now}
	if err := s.InsertTurns([]model.Turn{agentTurn, userTurn}); err != nil {
		t.Fatal(err)
	}
	action := model.Action{ID: "act_pipeline", TurnID: agentTurn.ID, ToolName: "shell", Command: "npm install", Excerpt: "npm install"}
	if err := s.InsertActions([]model.Action{action}); err != nil {
		t.Fatal(err)
	}
	normalized := privacy.NormalizeSignal("Use bun, not npm.")
	for i := 0; i < 2; i++ {
		correction := model.Correction{
			ID:         "cor_pipeline_" + string(rune('a'+i)),
			SessionID:  sess.ID,
			TurnID:     userTurn.ID,
			ActionID:   action.ID,
			Hash:       privacy.HashSignal(normalized),
			Normalized: normalized,
			Excerpt:    "Use bun, not npm.",
			Agent:      "claude",
			Project:    "repo",
			CreatedAt:  now.Add(time.Duration(i) * time.Minute),
		}
		if err := s.InsertCorrection(correction); err != nil {
			t.Fatal(err)
		}
	}
}
