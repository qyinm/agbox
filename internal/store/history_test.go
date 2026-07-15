package store

import (
	"path/filepath"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/model"
)

func TestHistoryWindowDefaultsAndPersists(t *testing.T) {
	path := filepath.Join(t.TempDir(), "agbox.db")
	s, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	if got, err := s.HistoryWindow(); err != nil || got != DefaultHistoryWindow {
		t.Fatalf("default history window = %v, %v; want %v", got, err, DefaultHistoryWindow)
	}
	if err := s.SetHistoryWindow(30 * 24 * time.Hour); err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	s, err = Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	if got, err := s.HistoryWindow(); err != nil || got != 30*24*time.Hour {
		t.Fatalf("persisted history window = %v, %v", got, err)
	}
	if err := s.SetHistoryWindow(0); err == nil {
		t.Fatal("zero history window accepted")
	}
}

func TestApplyEvidenceAgingReplacesLinksBlanksContentAndDeactivates(t *testing.T) {
	s, err := Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	now := time.Date(2026, 7, 16, 12, 0, 0, 0, time.UTC)
	if err := s.SetHistoryWindow(30 * 24 * time.Hour); err != nil {
		t.Fatal(err)
	}
	seedHistoryCandidate(t, s, now, []struct {
		id string
		at time.Time
	}{
		{id: "cor_expired", at: now.Add(-30*time.Hour*24 - time.Nanosecond)},
		{id: "cor_boundary", at: now.Add(-30 * time.Hour * 24)},
	})

	result, err := s.ApplyEvidenceAging(now, 2)
	if err != nil {
		t.Fatal(err)
	}
	if result.ExpiredCorrections != 1 || result.DeactivatedCandidates != 1 {
		t.Fatalf("aging result = %+v", result)
	}
	active, err := s.CorrectionsForCandidateAt("cand_history", now)
	if err != nil {
		t.Fatal(err)
	}
	if len(active) != 1 || active[0].ID != "cor_boundary" {
		t.Fatalf("active corrections = %+v", active)
	}
	all, err := s.ListCorrections()
	if err != nil {
		t.Fatal(err)
	}
	for _, cor := range all {
		if cor.ID == "cor_expired" && (cor.Normalized != "" || cor.Excerpt != "") {
			t.Fatalf("expired correction retained content: %+v", cor)
		}
	}
	candidate, err := s.GetCandidate("cand_history")
	if err != nil {
		t.Fatal(err)
	}
	if candidate.State != model.CandidateInactive || candidate.EventCount != 1 || candidate.Confidence != "insufficient" {
		t.Fatalf("aged candidate = %+v", candidate)
	}
}

func seedHistoryCandidate(t *testing.T, s *Store, now time.Time, corrections []struct {
	id string
	at time.Time
}) {
	t.Helper()
	session := model.Session{ID: "ses_history", Agent: "codex", Project: "agbox", SourcePath: "/tmp/history.jsonl", SourceHash: "h", StartedAt: now, UpdatedAt: now}
	if err := s.UpsertSession(session); err != nil {
		t.Fatal(err)
	}
	if err := s.InsertTurns([]model.Turn{{ID: "turn_history", SessionID: session.ID, TurnIndex: 1, Role: "user", EventType: "message", CreatedAt: now}}); err != nil {
		t.Fatal(err)
	}
	if err := s.InsertActions([]model.Action{{ID: "act_history", TurnID: "turn_history", ToolName: "shell", Command: "npm install", Excerpt: "npm install"}}); err != nil {
		t.Fatal(err)
	}
	ids := make([]string, 0, len(corrections))
	for _, item := range corrections {
		cor := model.Correction{ID: item.id, SessionID: session.ID, TurnID: "turn_history", ActionID: "act_history", Hash: "hash_" + item.id, Normalized: "use bun", Excerpt: "Use bun", Agent: "codex", Project: "agbox", CreatedAt: item.at}
		if err := s.InsertCorrection(cor); err != nil {
			t.Fatal(err)
		}
		ids = append(ids, item.id)
	}
	candidate := model.Candidate{ID: "cand_history", Fingerprint: "fp_history", Name: "history", Description: "history", RuleText: "use bun", SourceKind: model.CandidateSourceCorrection, State: model.CandidateProposalReady, EventCount: len(ids), ProjectCount: 1, SourceCount: 1, FirstSeen: corrections[0].at, LastSeen: corrections[len(corrections)-1].at, Confidence: "medium", Version: 1, UpdatedAt: now}
	if err := s.UpsertCandidate(candidate, nil, ids); err != nil {
		t.Fatal(err)
	}
}
