package cli

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/pipeline"
	"github.com/hippoom/agbox/internal/privacy"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/telemetry"
)

func TestExecuteEndToEndPromotionLoop(t *testing.T) {
	root := t.TempDir()
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chdir(root); err != nil {
		t.Fatal(err)
	}
	defer os.Chdir(wd)

	for i := 0; i < 2; i++ {
		if err := Execute([]string{"capture", "--project", "demo", "Use bun, not npm."}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
			t.Fatal(err)
		}
	}
	var out bytes.Buffer
	if err := Execute([]string{"scan"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	candidateID := "cand_" + privacy.HashSignal("prompt_pattern:semantic:package-manager:bun-over-npm")[:12]
	if !strings.Contains(out.String(), candidateID) {
		t.Fatalf("scan output %q does not include %s", out.String(), candidateID)
	}
	if err := Execute([]string{"approve", candidateID, "--name", "package-manager-workflow"}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	out.Reset()
	if err := Execute([]string{"export", candidateID, "--dry-run"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), `"path": "AGENTS.md"`) {
		t.Fatalf("dry-run output = %s", out.String())
	}
	out.Reset()
	var stderr bytes.Buffer
	if err := Execute([]string{"export", candidateID}, strings.NewReader(""), &out, &stderr); err != nil {
		t.Fatal(err)
	}
	if strings.Contains(out.String(), "undo: agbox export rollback exp_") {
		t.Fatalf("export stdout included rollback command:\n%s", out.String())
	}
	if !strings.Contains(stderr.String(), "undo: agbox export rollback exp_") {
		t.Fatalf("export stderr missing rollback command:\n%s", stderr.String())
	}
	if _, err := os.Stat(filepath.Join(root, "AGENTS.md")); err != nil {
		t.Fatal(err)
	}
	if err := Execute([]string{"manifest", "verify"}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if err := Execute([]string{"impact", candidateID}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
}

func TestInitShowsNextSteps(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chdir(root); err != nil {
		t.Fatal(err)
	}
	defer os.Chdir(wd)

	var out bytes.Buffer
	if err := Execute([]string{"init"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"Initialized agbox",
		"Next steps:",
		"managed hooks:",
		"telemetry: on by default",
		"agbox beta              # See setup + curated candidates in one terminal summary",
		"agbox doctor            # Check watcher + managed proposal hooks",
		"agbox disconnect <agent>",
		"agbox status            # Check watcher and sync status",
		"agbox demo              # See the workflow in action",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("init output missing %q:\n%s", want, got)
		}
	}
}

func TestInitCanSkipManagedHooks(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))
	t.Setenv("AGBOX_SKIP_CONNECT", "1")
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chdir(root); err != nil {
		t.Fatal(err)
	}
	defer os.Chdir(wd)

	var out bytes.Buffer
	if err := Execute([]string{"init"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	if !strings.Contains(got, "managed hooks skipped") {
		t.Fatalf("init skip output missing hook opt-out:\n%s", got)
	}
	if _, err := os.Stat(filepath.Join(root, ".claude", "settings.json")); !os.IsNotExist(err) {
		t.Fatalf("managed hooks were written despite AGBOX_SKIP_CONNECT=1: %v", err)
	}
}

func TestInitStopsWatcherBeforeSyncAndInstallsAfter(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))
	t.Setenv("AGBOX_SKIP_CONNECT", "1")
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chdir(root); err != nil {
		t.Fatal(err)
	}
	defer os.Chdir(wd)

	oldStopWatcher := stopWatcher
	oldInstallWatcher := installWatcher
	oldSyncBestEffort := syncBestEffort
	t.Cleanup(func() {
		stopWatcher = oldStopWatcher
		installWatcher = oldInstallWatcher
		syncBestEffort = oldSyncBestEffort
	})

	var steps []string
	stopWatcher = func(home string) error {
		if home != root {
			t.Fatalf("stop home = %q, want %q", home, root)
		}
		steps = append(steps, "stop")
		return nil
	}
	syncBestEffort = func(*store.Store) (pipeline.BestEffortSyncResult, error) {
		steps = append(steps, "sync")
		return pipeline.BestEffortSyncResult{}, nil
	}
	installWatcher = func(home, agboxBin string) error {
		if home != root {
			t.Fatalf("install home = %q, want %q", home, root)
		}
		if agboxBin == "" {
			t.Fatal("install agboxBin is empty")
		}
		steps = append(steps, "install")
		return nil
	}

	if err := Execute([]string{"init", "--quiet"}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	want := []string{"stop", "sync", "install"}
	if strings.Join(steps, ",") != strings.Join(want, ",") {
		t.Fatalf("init steps = %v, want %v", steps, want)
	}
}

func TestCommandHelpDoesNotOpenStore(t *testing.T) {
	root := t.TempDir()
	dbPath := filepath.Join(root, "agbox.db")
	t.Setenv("AGBOX_DB", dbPath)

	var out bytes.Buffer
	if err := Execute([]string{"capture", "--help"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"Usage:",
		"agbox capture",
		"--agent name",
		"reads from stdin",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("capture help missing %q:\n%s", want, got)
		}
	}
	if _, err := os.Stat(dbPath); !os.IsNotExist(err) {
		t.Fatalf("capture help opened store: %v", err)
	}
}

func TestReviewHelpDoesNotOpenStore(t *testing.T) {
	root := t.TempDir()
	dbPath := filepath.Join(root, "agbox.db")
	t.Setenv("AGBOX_DB", dbPath)

	var out bytes.Buffer
	if err := Execute([]string{"review", "--help"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"agbox review",
		"--state state",
		"proposal_ready",
		"--min-repeats n",
		"--limit n",
		"interactive review UI",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("review help missing %q:\n%s", want, got)
		}
	}
	if _, err := os.Stat(dbPath); !os.IsNotExist(err) {
		t.Fatalf("review help opened store: %v", err)
	}
}

func TestBetaHelpDocumentsSyncFlag(t *testing.T) {
	root := t.TempDir()
	dbPath := filepath.Join(root, "agbox.db")
	t.Setenv("AGBOX_DB", dbPath)

	var out bytes.Buffer
	if err := Execute([]string{"beta", "--help"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"agbox beta [--limit 5] [--sync]",
		"--sync",
		"Force session ingest",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("beta help missing %q:\n%s", want, got)
		}
	}
	if _, err := os.Stat(dbPath); !os.IsNotExist(err) {
		t.Fatalf("beta help opened store: %v", err)
	}
}

func TestReviewProposalStateIsValidBeforeTerminalCheck(t *testing.T) {
	root := t.TempDir()
	dbPath := filepath.Join(root, "agbox.db")
	t.Setenv("AGBOX_DB", dbPath)

	err := Execute([]string{"review", "--state", "proposal_ready"}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{})
	if err == nil {
		t.Fatal("review succeeded with non-interactive stdio")
	}
	if strings.Contains(err.Error(), "--state must be") {
		t.Fatalf("proposal_ready rejected as invalid state: %v", err)
	}
	if !strings.Contains(err.Error(), "requires an interactive terminal") {
		t.Fatalf("review error = %q, want terminal error", err.Error())
	}
	if _, err := os.Stat(dbPath); !os.IsNotExist(err) {
		t.Fatalf("review noninteractive opened store: %v", err)
	}
}

func TestReviewNonInteractiveReturnsTerminalError(t *testing.T) {
	root := t.TempDir()
	dbPath := filepath.Join(root, "agbox.db")
	t.Setenv("AGBOX_DB", dbPath)

	err := Execute([]string{"review"}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{})
	if err == nil {
		t.Fatal("review succeeded with non-interactive stdio")
	}
	want := "agbox review requires an interactive terminal; use agbox discover or agbox inbox instead"
	if err.Error() != want {
		t.Fatalf("review error = %q, want %q", err.Error(), want)
	}
	if _, err := os.Stat(dbPath); !os.IsNotExist(err) {
		t.Fatalf("review noninteractive opened store: %v", err)
	}
}

func TestStatusFreshHomeShowsZeroCounts(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	var out bytes.Buffer
	if err := Execute([]string{"status"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"watcher: stopped",
		"last sync: never",
		"corrections: 0",
		"candidates: 0",
		"managed hooks:",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("status output missing %q:\n%s", want, got)
		}
	}
}

func TestStatusReconcilesExistingSkillFile(t *testing.T) {
	root := t.TempDir()
	dbPath := filepath.Join(root, "agbox.db")
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", dbPath)
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chdir(root); err != nil {
		t.Fatal(err)
	}
	defer os.Chdir(wd)

	s, err := store.Open(dbPath)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Now()
	c := model.Candidate{
		ID:          "cand_status123",
		Fingerprint: "fp_status123",
		Name:        "status-skill",
		State:       model.CandidateProposed,
		EventCount:  3,
		ProposedAt:  now,
		FirstSeen:   now,
		LastSeen:    now,
		UpdatedAt:   now,
	}
	if err := s.UpsertCandidate(c, nil, nil); err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	skillDir := filepath.Join(root, ".agents", "skills", "status-skill")
	if err := os.MkdirAll(skillDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte("---\nname: status-skill\nagbox_candidate_id: cand_status123\n---\nbody\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := Execute([]string{"status"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), "accepted skills: 1 reconciled") {
		t.Fatalf("status output missing reconcile line:\n%s", out.String())
	}

	s, err = store.Open(dbPath)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	got, err := s.GetCandidate(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.State != model.CandidateAccepted {
		t.Fatalf("state = %s, want accepted", got.State)
	}
}

func TestDoctorFreshHomeShowsRepairGuidance(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	var out bytes.Buffer
	if err := Execute([]string{"doctor"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"watcher: not installed",
		"hook claude:",
		"next: agbox init",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("doctor output missing %q:\n%s", want, got)
		}
	}
}

func TestHelpCommandShowsCommandHelp(t *testing.T) {
	var out bytes.Buffer
	if err := Execute([]string{"help", "status"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"agbox status",
		"watcher state",
		"last sync",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("status help missing %q:\n%s", want, got)
		}
	}
}

func TestHookProposeRequiresAgent(t *testing.T) {
	root := t.TempDir()
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))
	err := Execute([]string{"hook", "propose"}, strings.NewReader("{}"), &bytes.Buffer{}, &bytes.Buffer{})
	if err == nil {
		t.Fatal("hook propose without agent should fail")
	}
	if !strings.Contains(err.Error(), "usage: agbox hook propose") {
		t.Fatalf("hook error = %q", err.Error())
	}
}

func TestBetaEmptyStoreShowsSetupAndDemo(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	var out bytes.Buffer
	if err := Execute([]string{"beta"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"agbox beta",
		"watcher:",
		"managed hooks:",
		"No strong workflow candidates yet.",
		"agbox demo",
		"agbox doctor",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("beta empty output missing %q:\n%s", want, got)
		}
	}
}

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
	if !strings.Contains(got, "No strong workflow candidates yet.") {
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
		"Workflow candidates",
		"package-manager-workflow",
		"state=proposal_ready",
		"repeats=2",
		"example: ran `npm install` -> Use bun, not npm.",
		"agbox evidence cand_",
		"ready to propose inside your agent",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("beta candidate output missing %q:\n%s", want, got)
		}
	}
}

func TestBetaLimitZeroDoesNotClaimNoCorrections(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))
	seedBetaCorrectionCandidate(t, filepath.Join(root, "agbox.db"))

	var out bytes.Buffer
	if err := Execute([]string{"beta", "--limit", "0"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	if !strings.Contains(got, "Candidate display disabled by --limit 0.") {
		t.Fatalf("beta limit 0 output missing setup-only copy:\n%s", got)
	}
	if strings.Contains(got, "No repeated corrections yet.") {
		t.Fatalf("beta limit 0 falsely claimed no corrections:\n%s", got)
	}
}

func TestDiscoverShowsCandidateEvidenceAndNextCommands(t *testing.T) {
	root := t.TempDir()
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))
	for _, agent := range []string{"codex", "claude"} {
		if err := Execute([]string{"capture", "--agent", agent, "Use bun, not npm."}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
			t.Fatal(err)
		}
	}

	var out bytes.Buffer
	if err := Execute([]string{"discover"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"Workflow candidates",
		"repeats=2",
		"excerpt: Use bun, not npm.",
		"agbox evidence cand_",
		"agbox approve cand_",
		"agbox export cand_",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("discover output missing %q:\n%s", want, got)
		}
	}
}

func TestDiscoverEmptyShowsHookSetupNextStep(t *testing.T) {
	root := t.TempDir()
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))
	t.Setenv("HOME", root)

	var out bytes.Buffer
	if err := Execute([]string{"discover"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"No workflow candidates yet.",
		"Claude, Codex, Cursor, or Grok",
		"agbox beta",
		"agbox status",
		"agbox review",
		"agbox demo",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("discover empty output missing %q:\n%s", want, got)
		}
	}
}

func TestDemoShowsPreviewWithoutPersistentStore(t *testing.T) {
	root := t.TempDir()
	dbPath := filepath.Join(root, "agbox.db")
	t.Setenv("AGBOX_DB", dbPath)

	var out bytes.Buffer
	if err := Execute([]string{"demo"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"agbox demo: repeated workflow signal detected",
		"Skill preview:",
		"use bun, not npm",
		"evidence:",
		"No files were changed",
		"agbox beta",
		"agbox review",
		"agbox status",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("demo output missing %q:\n%s", want, got)
		}
	}
	if _, err := os.Stat(dbPath); !os.IsNotExist(err) {
		t.Fatalf("demo touched persistent AGBOX_DB: %v", err)
	}
}

func setupTelemetryHome(t *testing.T) string {
	t.Helper()
	home := filepath.Join(t.TempDir(), ".agbox")
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("AGBOX_HOME", home)
	t.Setenv("AGBOX_TELEMETRY", "")
	return home
}

func TestTelemetryStatusDefaultOn(t *testing.T) {
	setupTelemetryHome(t)

	var out bytes.Buffer
	if err := Execute([]string{"telemetry", "status"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"telemetry: on (not configured",
		"agbox telemetry off",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("telemetry status output missing %q:\n%s", want, got)
		}
	}
}

func TestTelemetryOnEnablesAfterOptOut(t *testing.T) {
	setupTelemetryHome(t)
	t.Setenv("POSTHOG_API_KEY", "phc_test")
	if err := telemetry.OptOut(); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := Execute([]string{"telemetry", "on"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"agbox_install_completed",
		"agbox_daily_active",
		"PostHog",
		"random UUID",
		"telemetry enabled",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("telemetry on output missing %q:\n%s", want, got)
		}
	}
	st, err := telemetry.LoadState()
	if err != nil {
		t.Fatal(err)
	}
	if !st.Enabled {
		t.Fatal("telemetry should be enabled after telemetry on")
	}
	if st.AnonymousID == "" {
		t.Fatal("expected anonymous_id after re-enable")
	}
	if !telemetry.Enabled() {
		t.Fatal("telemetry.Enabled() should be true after re-enable")
	}
}

func TestShouldRecordDailyActive(t *testing.T) {
	tests := []struct {
		args []string
		want bool
	}{
		{args: nil, want: false},
		{args: []string{}, want: false},
		{args: []string{"telemetry", "status"}, want: false},
		{args: []string{"help"}, want: false},
		{args: []string{"help", "status"}, want: false},
		{args: []string{"-h"}, want: false},
		{args: []string{"--help"}, want: false},
		{args: []string{"-v"}, want: false},
		{args: []string{"--version"}, want: false},
		{args: []string{"version"}, want: false},
		{args: []string{"capture", "--help"}, want: false},
		{args: []string{"init"}, want: false},
		{args: []string{"status"}, want: true},
		{args: []string{"doctor"}, want: true},
		{args: []string{"demo"}, want: true},
	}
	for _, tc := range tests {
		got := shouldRecordDailyActive(tc.args)
		if got != tc.want {
			t.Fatalf("shouldRecordDailyActive(%v) = %v, want %v", tc.args, got, tc.want)
		}
	}
}

func TestExecuteSkipsDailyActiveForTelemetry(t *testing.T) {
	setupTelemetryHome(t)

	var calls int
	old := maybeRecordDailyActiveHook
	maybeRecordDailyActiveHook = func() { calls++ }
	t.Cleanup(func() { maybeRecordDailyActiveHook = old })

	if err := Execute([]string{"telemetry", "status"}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if calls != 0 {
		t.Fatalf("telemetry status hook calls = %d, want 0", calls)
	}
}

func TestExecuteRecordsDailyActiveOnSuccess(t *testing.T) {
	root := t.TempDir()
	t.Setenv("HOME", root)
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	var calls int
	old := maybeRecordDailyActiveHook
	maybeRecordDailyActiveHook = func() { calls++ }
	t.Cleanup(func() { maybeRecordDailyActiveHook = old })

	if err := Execute([]string{"status"}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if calls != 1 {
		t.Fatalf("status hook calls = %d, want 1", calls)
	}
}

func TestExecuteSkipsDailyActiveOnError(t *testing.T) {
	var calls int
	old := maybeRecordDailyActiveHook
	maybeRecordDailyActiveHook = func() { calls++ }
	t.Cleanup(func() { maybeRecordDailyActiveHook = old })

	err := Execute([]string{"unknown-cmd"}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{})
	if err == nil {
		t.Fatal("expected error for unknown command")
	}
	if calls != 0 {
		t.Fatalf("failed command hook calls = %d, want 0", calls)
	}
}

func TestExecuteOptedOutDoesNotPanic(t *testing.T) {
	setupTelemetryHome(t)
	root := t.TempDir()
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))

	if err := Execute([]string{"status"}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
}

func TestTelemetryOffDisables(t *testing.T) {
	setupTelemetryHome(t)
	t.Setenv("POSTHOG_API_KEY", "phc_test")
	if _, err := telemetry.OptIn(); err != nil {
		t.Fatal(err)
	}
	if !telemetry.Enabled() {
		t.Fatal("telemetry should be enabled before off")
	}

	var out bytes.Buffer
	if err := Execute([]string{"telemetry", "off"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), "telemetry disabled") {
		t.Fatalf("telemetry off output = %q", out.String())
	}
	st, err := telemetry.LoadState()
	if err != nil {
		t.Fatal(err)
	}
	if st.Enabled {
		t.Fatal("telemetry should be disabled after off")
	}
	if telemetry.Enabled() {
		t.Fatal("telemetry.Enabled() should be false after off")
	}
}

func seedBetaCorrectionCandidate(t *testing.T, dbPath string) {
	t.Helper()
	s, err := store.Open(dbPath)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	now := time.Now()
	sess := model.Session{
		ID:         "sess_beta",
		Agent:      "claude",
		Project:    "repo",
		SourcePath: filepath.Join(filepath.Dir(dbPath), "session.jsonl"),
		SourceHash: "hash",
		StartedAt:  now,
		UpdatedAt:  now,
	}
	if err := s.UpsertSession(sess); err != nil {
		t.Fatal(err)
	}
	agentTurn := model.Turn{ID: "turn_agent", SessionID: sess.ID, TurnIndex: 1, Role: "agent", EventType: "tool", CreatedAt: now}
	userTurn := model.Turn{ID: "turn_user", SessionID: sess.ID, TurnIndex: 2, Role: "user", EventType: "message", CreatedAt: now}
	if err := s.InsertTurns([]model.Turn{agentTurn, userTurn}); err != nil {
		t.Fatal(err)
	}
	action := model.Action{ID: "act_beta", TurnID: agentTurn.ID, ToolName: "shell", Command: "npm install", Excerpt: "npm install"}
	if err := s.InsertActions([]model.Action{action}); err != nil {
		t.Fatal(err)
	}
	normalized := privacy.NormalizeSignal("Use bun, not npm.")
	for i := 0; i < 2; i++ {
		correction := model.Correction{
			ID:         "cor_beta_" + string(rune('a'+i)),
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
	if _, err := scan.Run(s, 2); err != nil {
		t.Fatal(err)
	}
	candidates, err := s.ListCandidates("")
	if err != nil {
		t.Fatal(err)
	}
	if len(candidates) != 1 {
		t.Fatalf("candidates = %d, want 1", len(candidates))
	}
	if err := s.UpdateCandidateMeta(candidates[0].ID, store.CandidateMetaUpdate{State: model.CandidateProposalReady}); err != nil {
		t.Fatal(err)
	}
}

func seedBetaCandidate(t *testing.T, s *store.Store, c model.Candidate) model.Candidate {
	t.Helper()
	now := time.Now()
	if c.Fingerprint == "" {
		c.Fingerprint = privacy.HashSignal(c.Name + "|" + c.RuleText + "|" + string(c.SourceKind))
	}
	if c.ID == "" {
		c.ID = "cand_" + c.Fingerprint[:12]
	}
	if c.Description == "" {
		c.Description = "test candidate"
	}
	if c.State == "" {
		c.State = model.CandidateProposalReady
	}
	if c.EventCount == 0 {
		c.EventCount = 1
	}
	if c.ProjectCount == 0 {
		c.ProjectCount = 1
	}
	if c.SourceCount == 0 {
		c.SourceCount = 1
	}
	if c.Confidence == "" {
		c.Confidence = "medium"
	}
	if c.FirstSeen.IsZero() {
		c.FirstSeen = now
	}
	if c.LastSeen.IsZero() {
		c.LastSeen = now
	}
	if c.UpdatedAt.IsZero() {
		c.UpdatedAt = now
	}
	if c.Version == 0 {
		c.Version = 1
	}
	if err := s.UpsertCandidate(c, nil, nil); err != nil {
		t.Fatal(err)
	}
	return c
}
