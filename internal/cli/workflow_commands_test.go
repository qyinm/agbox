package cli

import (
	"bytes"
	"errors"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/pipeline"
	"github.com/hippoom/agbox/internal/store"
)

func TestBetaCandidatesHideGeneratedPromptNoise(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	good := seedBetaCandidate(t, s, model.Candidate{
		Name:         "package-manager-workflow",
		RuleText:     "Use bun, not npm.",
		SemanticKey:  "package-manager:bun-over-npm",
		SourceKind:   model.CandidateSourceCorrection,
		State:        model.CandidateProposalReady,
		EventCount:   2,
		ProjectCount: 1,
		SourceCount:  1,
		Confidence:   "medium",
	})
	seedBetaCandidate(t, s, model.Candidate{
		Name:         "generated-suggestion-boilerplate",
		RuleText:     "Generate 0 to 3 hyperpersonalized suggestions for the user based on their recent prompts.",
		SourceKind:   model.CandidateSourcePromptPattern,
		State:        model.CandidateProposalReady,
		EventCount:   20,
		ProjectCount: 3,
		SourceCount:  2,
		Confidence:   "high",
	})

	candidates, err := betaCandidates(s, 5)
	if err != nil {
		t.Fatal(err)
	}
	if len(candidates) != 1 {
		t.Fatalf("candidates = %d, want 1", len(candidates))
	}
	if candidates[0].ID != good.ID {
		t.Fatalf("candidate = %s, want %s", candidates[0].ID, good.ID)
	}
}

func TestBetaCandidatesPrioritizeSemanticPromptOverFileWrapper(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	currentProject := seedBetaCandidate(t, s, model.Candidate{
		Name:         "current-project-analysis-workflow",
		RuleText:     "현재 프로젝트 분석해줘.",
		SemanticKey:  "current-project-analysis",
		SourceKind:   model.CandidateSourcePromptPattern,
		State:        model.CandidateProposalReady,
		EventCount:   3,
		ProjectCount: 2,
		SourceCount:  1,
		Confidence:   "high",
	})
	seedBetaCandidate(t, s, model.Candidate{
		Name:         "files-mentioned-by-the-user-codex-clipboard",
		RuleText:     "# Files mentioned by the user:\n\n- /Users/demo/Desktop/Codex Clipboard.txt",
		SourceKind:   model.CandidateSourcePromptPattern,
		State:        model.CandidateProposalReady,
		EventCount:   30,
		ProjectCount: 5,
		SourceCount:  2,
		Confidence:   "high",
	})

	candidates, err := betaCandidates(s, 5)
	if err != nil {
		t.Fatal(err)
	}
	if len(candidates) != 1 {
		t.Fatalf("candidates = %d, want 1", len(candidates))
	}
	if candidates[0].ID != currentProject.ID {
		t.Fatalf("candidate = %s, want %s", candidates[0].ID, currentProject.ID)
	}
}

func TestBetaHiddenCandidatesShowNoStrongCandidates(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	s, err := store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	seedBetaCandidate(t, s, model.Candidate{
		Name:         "files-mentioned-by-the-user-codex-clipboard",
		RuleText:     "# Files mentioned by the user:\n\n- /Users/demo/Desktop/Codex Clipboard.txt",
		SourceKind:   model.CandidateSourcePromptPattern,
		State:        model.CandidateProposalReady,
		EventCount:   30,
		ProjectCount: 5,
		SourceCount:  2,
		Confidence:   "high",
	})
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := Execute([]string{"beta"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	if !strings.Contains(got, "No strong Recorded Workflows yet.") {
		t.Fatalf("beta output missing no-strong-candidates message:\n%s", got)
	}
	if strings.Contains(got, "files-mentioned-by-the-user") {
		t.Fatalf("beta output showed hidden noise candidate:\n%s", got)
	}
}

func TestBetaShowsCandidateCausalEvidenceAndNextAction(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))
	seedBetaCorrectionCandidate(t, filepath.Join(root, "agbox.db"))

	var out bytes.Buffer
	if err := Execute([]string{"beta"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"Recorded Workflows",
		"package-manager-workflow",
		"state=proposal_ready",
		"repeats=2",
		"example: ran `npm install` -> Use bun, not npm.",
		"agbox evidence cand_",
		"ready to propose inside your agent",
		"Manage recorded workflows: agbox inbox",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("beta candidate output missing %q:\n%s", want, got)
		}
	}
}

func TestInboxEmptyShowsRecordedWorkflowOnboarding(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	var out bytes.Buffer
	if err := Execute([]string{"inbox"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"No Recorded Workflows yet.",
		"agbox demo",
		"agbox doctor",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("inbox empty output missing %q:\n%s", want, got)
		}
	}
	if strings.Contains(got, "Promotion Inbox") {
		t.Fatalf("inbox empty output still uses promotion copy:\n%s", got)
	}
}

func TestInboxShowsRecordedWorkflowCard(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	s, err := store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	seedBetaCandidate(t, s, model.Candidate{
		Name:         "current-project-analysis-workflow",
		RuleText:     "현재 프로젝트 분석해줘.",
		SemanticKey:  "current-project-analysis",
		SourceKind:   model.CandidateSourcePromptPattern,
		State:        model.CandidateProposalReady,
		EventCount:   3,
		ProjectCount: 1,
		SourceCount:  1,
		Confidence:   "high",
	})
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := Execute([]string{"inbox"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"Recorded Workflows",
		"Current Project Analysis",
		"lifecycle=Ready to replay",
		"when:",
		"replay:",
		"Inspect repository structure",
		"evidence:",
		"safety:",
		"does not re-run prior commands",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("inbox card missing %q:\n%s", want, got)
		}
	}
	if strings.Contains(got, "Promotion Inbox") {
		t.Fatalf("inbox card still uses promotion copy:\n%s", got)
	}
}

func TestInboxFiltersAppliedOnce(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	s, err := store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	seedBetaCandidate(t, s, model.Candidate{
		Name:         "current-project-analysis-workflow",
		RuleText:     "현재 프로젝트 분석해줘.",
		SemanticKey:  "current-project-analysis",
		SourceKind:   model.CandidateSourcePromptPattern,
		State:        model.CandidateAppliedOnce,
		EventCount:   3,
		ProjectCount: 1,
		SourceCount:  1,
		Confidence:   "high",
	})
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := Execute([]string{"inbox", "--state", "applied_once"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	if !strings.Contains(got, "lifecycle=Applied once") {
		t.Fatalf("inbox applied_once output missing lifecycle:\n%s", got)
	}
}

func TestBetaShowsSaveSuggestedNextAction(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	s, err := store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	c := seedBetaCandidate(t, s, model.Candidate{
		Name:         "current-project-analysis-workflow",
		RuleText:     "현재 프로젝트 분석해줘.",
		SemanticKey:  "current-project-analysis",
		SourceKind:   model.CandidateSourcePromptPattern,
		State:        model.CandidateSaveSuggested,
		EventCount:   3,
		ProjectCount: 1,
		SourceCount:  1,
		Confidence:   "high",
	})
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := Execute([]string{"beta"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"state=save_suggested",
		"answer the save-for-future prompt, or run agbox snooze " + c.ID,
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("beta save_suggested output missing %q:\n%s", want, got)
		}
	}
}

func TestHookReplayUsesPromptMatchAndMarksProposed(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	s, err := store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	c := seedBetaCandidate(t, s, model.Candidate{
		Name:         "current-project-analysis-workflow",
		RuleText:     "현재 프로젝트 분석해줘.",
		SemanticKey:  "current-project-analysis",
		SourceKind:   model.CandidateSourcePromptPattern,
		State:        model.CandidateProposalReady,
		EventCount:   3,
		ProjectCount: 1,
		SourceCount:  1,
		Confidence:   "high",
	})
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	hookData := `{"cwd":"` + filepath.Join(root, "agbox") + `","prompt":"현재 프로젝트 분석해줘"}`
	if err := Execute([]string{"hook", "replay", "codex"}, strings.NewReader(hookData), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	if !strings.Contains(got, c.ID) {
		t.Fatalf("replay output missing candidate id %s:\n%s", c.ID, got)
	}
	if !strings.Contains(got, "agbox recorded workflow replay instructions") {
		t.Fatalf("replay output missing replay instructions:\n%s", got)
	}
	if strings.Contains(got, "agbox skill proposal instructions") {
		t.Fatalf("replay output used skill proposal payload:\n%s", got)
	}
	s, err = store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	stored, err := s.GetCandidate(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if stored.State != model.CandidateProposed {
		t.Fatalf("state = %s, want proposed", stored.State)
	}
}

func TestHookReplayDoesNotRunStaleSync(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	oldSyncBestEffortIfStale := syncBestEffortIfStale
	t.Cleanup(func() {
		syncBestEffortIfStale = oldSyncBestEffortIfStale
	})
	syncBestEffortIfStale = func(*store.Store) (pipeline.BestEffortSyncResult, error) {
		return pipeline.BestEffortSyncResult{}, errors.New("unexpected sync from replay hook")
	}

	s, err := store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	c := seedBetaCandidate(t, s, model.Candidate{
		Name:         "current-project-analysis-workflow",
		RuleText:     "현재 프로젝트 분석해줘.",
		SemanticKey:  "current-project-analysis",
		SourceKind:   model.CandidateSourcePromptPattern,
		State:        model.CandidateProposalReady,
		EventCount:   3,
		ProjectCount: 1,
		SourceCount:  1,
		Confidence:   "high",
	})
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	hookData := `{"cwd":"` + filepath.Join(root, "agbox") + `","prompt":"현재 프로젝트 분석해줘"}`
	if err := Execute([]string{"hook", "replay", "codex"}, strings.NewReader(hookData), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), c.ID) {
		t.Fatalf("replay output missing candidate id %s:\n%s", c.ID, out.String())
	}
}

func TestApplyRecordsAppliedOnce(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	s, err := store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	c := seedBetaCandidate(t, s, model.Candidate{
		Name:         "current-project-analysis-workflow",
		RuleText:     "현재 프로젝트 분석해줘.",
		SemanticKey:  "current-project-analysis",
		SourceKind:   model.CandidateSourcePromptPattern,
		State:        model.CandidateProposed,
		EventCount:   3,
		ProjectCount: 1,
		SourceCount:  1,
		Confidence:   "high",
	})
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := Execute([]string{
		"apply", c.ID,
		"--agent", "codex",
		"--project", "agbox",
		"--prompt-hash", "hash123",
		"--prompt-excerpt", "현재 프로젝트 분석해줘",
	}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), c.ID+" -> applied once") {
		t.Fatalf("apply output = %q", out.String())
	}
	s, err = store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	stored, err := s.GetCandidate(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if stored.State != model.CandidateAppliedOnce {
		t.Fatalf("state = %s, want applied_once", stored.State)
	}
	apps, err := s.ListReplayApplications(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if len(apps) != 1 {
		t.Fatalf("applications = %d, want 1", len(apps))
	}
	if apps[0].Agent != "codex" || apps[0].Project != "agbox" || apps[0].PromptHash != "hash123" {
		t.Fatalf("application metadata = %+v", apps[0])
	}
}

func TestHookSavePromptsAfterAppliedOnce(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	s, err := store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	c := seedBetaCandidate(t, s, model.Candidate{
		Name:         "current-project-analysis-workflow",
		RuleText:     "현재 프로젝트 분석해줘.",
		SemanticKey:  "current-project-analysis",
		SourceKind:   model.CandidateSourcePromptPattern,
		State:        model.CandidateAppliedOnce,
		EventCount:   3,
		ProjectCount: 1,
		SourceCount:  1,
		Confidence:   "high",
	})
	if _, err := s.RecordReplayApplication(model.ReplayApplication{
		ID:          "rapp_hooksave",
		CandidateID: c.ID,
		Agent:       "codex",
		Project:     "agbox",
		AppliedAt:   time.Now(),
	}); err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := Execute([]string{"hook", "save", "codex"}, strings.NewReader(`{"cwd":"`+filepath.Join(root, "agbox")+`"}`), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	if !strings.Contains(got, "agbox save recorded workflow instructions") {
		t.Fatalf("save hook output missing save instructions:\n%s", got)
	}
	s, err = store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	stored, err := s.GetCandidate(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if stored.State != model.CandidateSaveSuggested {
		t.Fatalf("state = %s, want save_suggested", stored.State)
	}
}

func TestHookReplayWithoutPromptEmitsNothing(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	s, err := store.Open(filepath.Join(root, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	seedBetaCandidate(t, s, model.Candidate{
		Name:         "current-project-analysis-workflow",
		RuleText:     "현재 프로젝트 분석해줘.",
		SemanticKey:  "current-project-analysis",
		SourceKind:   model.CandidateSourcePromptPattern,
		State:        model.CandidateProposalReady,
		EventCount:   3,
		ProjectCount: 1,
		SourceCount:  1,
		Confidence:   "high",
	})
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := Execute([]string{"hook", "replay", "codex"}, strings.NewReader(`{"cwd":"`+root+`"}`), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if out.String() != "" {
		t.Fatalf("expected no replay output without prompt, got:\n%s", out.String())
	}
}
