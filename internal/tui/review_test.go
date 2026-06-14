package tui

import (
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"

	"github.com/hippoom/agbox/internal/capture"
	"github.com/hippoom/agbox/internal/model"
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
		"Next: agbox export cand_",
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
