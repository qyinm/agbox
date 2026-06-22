package cli

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/hippoom/agbox/internal/privacy"
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
	candidateID := "cand_" + privacy.HashSignal("semantic:package-manager:bun-over-npm")[:12]
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
	if err := Execute([]string{"export", candidateID}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
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
		"agbox review            # Review workflow candidates",
		"agbox status            # Check watcher and sync status",
		"agbox demo              # See the workflow in action",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("init output missing %q:\n%s", want, got)
		}
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

func TestHookCommandRemoved(t *testing.T) {
	err := Execute([]string{"hook", "codex"}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{})
	if err == nil {
		t.Fatal("hook command should be removed")
	}
	if !strings.Contains(err.Error(), `unknown command "hook"`) {
		t.Fatalf("hook error = %q", err.Error())
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
		"Use bun, not npm.",
		"No files were changed",
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
