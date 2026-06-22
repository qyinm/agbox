package tui

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/capture"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/store"
)

func TestReviewModelEmptyCandidates(t *testing.T) {
	s := openTestStore(t)
	defer s.Close()
	m := NewReviewModel(NewReviewService(s, ReviewOptions{})).Refresh()
	got := stripANSI(m.Render())
	for _, want := range []string{
		"No workflow candidates to review.",
		"agbox discover",
		"agbox capture",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("empty render missing %q:\n%s", want, got)
		}
	}
}

func TestReviewModelRendersCandidateAndEvidence(t *testing.T) {
	s := storeWithCandidates(t, "Use bun, not npm.", "Use bun, not npm.")
	defer s.Close()
	m := NewReviewModel(NewReviewService(s, ReviewOptions{})).Refresh()
	got := stripANSI(m.Render())
	for _, want := range []string{
		"package-manager-workflow",
		"pending",
		"confidence=low",
		"repeats=2",
		"Use bun, not npm.",
		"Reason",
		"Excerpts",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("candidate render missing %q:\n%s", want, got)
		}
	}
}

func TestReviewModelNavigationBoundaries(t *testing.T) {
	s := storeWithCandidates(t,
		"Use bun, not npm.",
		"Use bun, not npm.",
		"PR summary should include tests and risk.",
		"PR summary should include tests and risk.",
	)
	defer s.Close()
	m := NewReviewModel(NewReviewService(s, ReviewOptions{})).Refresh()
	if len(m.data.Candidates) != 2 {
		t.Fatalf("candidates = %d, want 2", len(m.data.Candidates))
	}
	first := m.cursor
	m = KeyForTest(m, "k")
	if m.cursor != first {
		t.Fatalf("cursor moved above first row: %d", m.cursor)
	}
	m = KeyForTest(m, "j")
	if m.cursor != 1 {
		t.Fatalf("cursor after j = %d, want 1", m.cursor)
	}
	m = KeyForTest(m, "j")
	if m.cursor != 1 {
		t.Fatalf("cursor moved past last row: %d", m.cursor)
	}
	m = KeyForTest(m, "k")
	if m.cursor != 0 {
		t.Fatalf("cursor after k = %d, want 0", m.cursor)
	}
}

func TestReviewModelApproveConfirm(t *testing.T) {
	s := storeWithCandidates(t, "Use bun, not npm.", "Use bun, not npm.")
	defer s.Close()
	m := NewReviewModel(NewReviewService(s, ReviewOptions{})).Refresh()
	id := m.data.Candidates[0].ID
	m = KeyForTest(m, "a")
	m = KeyForTest(m, "y")
	c, err := s.GetCandidate(id)
	if err != nil {
		t.Fatal(err)
	}
	if c.State != model.CandidateApproved {
		t.Fatalf("state = %s, want approved", c.State)
	}
}

func TestReviewModelRejectConfirm(t *testing.T) {
	s := storeWithCandidates(t, "Use bun, not npm.", "Use bun, not npm.")
	defer s.Close()
	m := NewReviewModel(NewReviewService(s, ReviewOptions{})).Refresh()
	id := m.data.Candidates[0].ID
	m = KeyForTest(m, "x")
	m = KeyForTest(m, "y")
	c, err := s.GetCandidate(id)
	if err != nil {
		t.Fatal(err)
	}
	if c.State != model.CandidateRejected {
		t.Fatalf("state = %s, want rejected", c.State)
	}
}

func TestReviewModelCancelLeavesStateUnchanged(t *testing.T) {
	s := storeWithCandidates(t, "Use bun, not npm.", "Use bun, not npm.")
	defer s.Close()
	m := NewReviewModel(NewReviewService(s, ReviewOptions{})).Refresh()
	id := m.data.Candidates[0].ID
	m = KeyForTest(m, "a")
	m = KeyForTest(m, "n")
	c, err := s.GetCandidate(id)
	if err != nil {
		t.Fatal(err)
	}
	if c.State != model.CandidatePending {
		t.Fatalf("state = %s, want pending", c.State)
	}
}

func TestReviewModelRefreshRunsScanAndReloads(t *testing.T) {
	s := openTestStore(t)
	defer s.Close()
	m := NewReviewModel(NewReviewService(s, ReviewOptions{})).Refresh()
	if len(m.data.Candidates) != 0 {
		t.Fatalf("initial candidates = %d, want 0", len(m.data.Candidates))
	}
	captureSignal(t, s, "Use bun, not npm.")
	captureSignal(t, s, "Use bun, not npm.")
	m = KeyForTest(m, "r")
	if len(m.data.Candidates) != 1 {
		t.Fatalf("candidates after refresh = %d, want 1", len(m.data.Candidates))
	}
}

func TestReviewModelDrillDownAndBack(t *testing.T) {
	s := storeWithCorrectionOccurrences(t)
	defer s.Close()
	m := NewReviewModel(NewReviewService(s, ReviewOptions{})).Refresh()
	if len(m.data.Candidates) == 0 {
		t.Fatal("expected candidates")
	}
	card := m.data.Cards[m.data.Candidates[0].ID]
	if len(card.Occurrences) == 0 {
		t.Fatal("expected occurrences")
	}
	m = KeyForTest(m, "enter")
	if m.view != viewDrillDown {
		t.Fatalf("view = %d, want drill-down", m.view)
	}
	m = KeyForTest(m, "esc")
	if m.view != viewList {
		t.Fatalf("view = %d, want list", m.view)
	}
}

func TestReviewModelProjectFilterToggle(t *testing.T) {
	s := storeWithCandidates(t, "Use bun, not npm.", "Use bun, not npm.")
	defer s.Close()
	service := NewReviewService(s, ReviewOptions{Project: "repo"})
	m := NewReviewModel(service).Refresh()
	withFilter := len(m.data.Candidates)
	m.showAllProjects = true
	m = m.Refresh()
	if len(m.data.Candidates) < withFilter {
		t.Fatalf("all projects count = %d, want >= %d", len(m.data.Candidates), withFilter)
	}
}

func TestReviewModelExportTargetPicker(t *testing.T) {
	s := storeWithCandidates(t, "Use bun, not npm.", "Use bun, not npm.")
	defer s.Close()
	m := NewReviewModel(NewReviewService(s, ReviewOptions{})).Refresh()
	m = KeyForTest(m, "a")
	m = KeyForTest(m, "y")
	m = KeyForTest(m, "e")
	if m.view != viewExportTarget {
		t.Fatalf("view = %d, want export target", m.view)
	}
	got := stripANSI(m.Render())
	if !strings.Contains(got, "Export target") || !strings.Contains(got, "AGENTS.md") {
		t.Fatalf("export picker missing labels:\n%s", got)
	}
}

func TestReviewActionsDoNotExportOrWriteProjectFiles(t *testing.T) {
	root := t.TempDir()
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chdir(root); err != nil {
		t.Fatal(err)
	}
	defer os.Chdir(wd)
	s, err := store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	captureSignal(t, s, "Use bun, not npm.")
	captureSignal(t, s, "Use bun, not npm.")
	m := NewReviewModel(NewReviewService(s, ReviewOptions{})).Refresh()
	m = KeyForTest(m, "a")
	m = KeyForTest(m, "y")
	stats, err := s.Stats()
	if err != nil {
		t.Fatal(err)
	}
	if stats.Exports != 0 {
		t.Fatalf("exports = %d, want 0", stats.Exports)
	}
	if _, err := os.Stat(filepath.Join(root, "AGENTS.md")); !os.IsNotExist(err) {
		t.Fatalf("review action wrote AGENTS.md: %v", err)
	}
}

func openTestStore(t *testing.T) *store.Store {
	t.Helper()
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	return s
}

func storeWithCorrectionOccurrences(t *testing.T) *store.Store {
	t.Helper()
	s := openTestStore(t)
	now := time.Now()
	normalized := privacy.NormalizeSignal("Use bun, not npm.")
	sess := model.Session{
		ID: "sess_review", Agent: "claude", Project: "repo",
		SourcePath: "/tmp/session.jsonl", SourceHash: "abc",
		StartedAt: now, UpdatedAt: now,
	}
	if err := s.UpsertSession(sess); err != nil {
		t.Fatal(err)
	}
	turnAgent := model.Turn{ID: "turn_a", SessionID: sess.ID, TurnIndex: 1, Role: "agent", EventType: "tool", CreatedAt: now}
	turnUser := model.Turn{ID: "turn_u", SessionID: sess.ID, TurnIndex: 2, Role: "user", EventType: "message", CreatedAt: now}
	if err := s.InsertTurns([]model.Turn{turnAgent, turnUser}); err != nil {
		t.Fatal(err)
	}
	action := model.Action{ID: "act_1", TurnID: turnAgent.ID, ToolName: "run_terminal_cmd", Command: "npm install", Excerpt: "npm install"}
	if err := s.InsertActions([]model.Action{action}); err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 2; i++ {
		correction := model.Correction{
			ID: fmt.Sprintf("cor_%d", i), SessionID: sess.ID, TurnID: turnUser.ID, ActionID: action.ID,
			Hash: privacy.HashSignal(normalized), Normalized: normalized, Excerpt: "Use bun, not npm.",
			Agent: "claude", Project: "repo", CreatedAt: now,
		}
		if err := s.InsertCorrection(correction); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := scan.Run(s, 2); err != nil {
		t.Fatal(err)
	}
	return s
}

func storeWithCandidates(t *testing.T, signals ...string) *store.Store {
	t.Helper()
	s := openTestStore(t)
	for _, signal := range signals {
		captureSignal(t, s, signal)
	}
	return s
}

func captureSignal(t *testing.T, s *store.Store, signal string) {
	t.Helper()
	if _, err := capture.Capture(s, signal, capture.Options{Project: "repo", Source: "manual", Agent: "codex", Redact: true}); err != nil {
		t.Fatal(err)
	}
}

var ansiPattern = regexp.MustCompile(`\x1b\[[0-9;]*m`)

func stripANSI(s string) string {
	return ansiPattern.ReplaceAllString(s, "")
}
