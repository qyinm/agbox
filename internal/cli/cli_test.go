package cli

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/hippoom/agbox/internal/privacy"
	"github.com/hippoom/agbox/internal/store"
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
		"agbox demo              # See the workflow in action",
		"agbox connect all --apply  # Connect to your AI agents",
		"agbox capture --agent codex \"Your workflow rule\"",
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
	if err := Execute([]string{"help", "connect"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	for _, want := range []string{
		"agbox connect <claude|codex|all>",
		"--command path",
		"Install agbox-managed hook config",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("connect help missing %q:\n%s", want, got)
		}
	}
}

func TestHookCaptureIsSilentAndStoresRedactedExcerpt(t *testing.T) {
	root := t.TempDir()
	dbPath := filepath.Join(root, "agbox.db")
	t.Setenv("AGBOX_DB", dbPath)
	var out bytes.Buffer
	payload := `{"prompt":"Use bun, not npm. email dev@example.com token=super-secret"}`
	if err := Execute([]string{"hook", "codex"}, strings.NewReader(payload), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if out.String() != "" {
		t.Fatalf("hook stdout = %q, want silent", out.String())
	}

	s, err := store.Open(dbPath)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	events, err := s.ListEvents()
	if err != nil {
		t.Fatal(err)
	}
	if len(events) != 1 {
		t.Fatalf("events = %d, want 1", len(events))
	}
	e := events[0]
	if e.Source != "hook" || e.Agent != "codex" {
		t.Fatalf("event source/agent = %s/%s", e.Source, e.Agent)
	}
	if e.RawStored || e.Raw != "" {
		t.Fatalf("hook stored raw prompt: raw_stored=%t raw=%q", e.RawStored, e.Raw)
	}
	if !strings.Contains(e.Excerpt, "[redacted-email]") || !strings.Contains(e.Excerpt, "token=[redacted]") {
		t.Fatalf("excerpt was not redacted: %q", e.Excerpt)
	}
	if strings.Contains(e.Excerpt, "dev@example.com") || strings.Contains(e.Normalized, "super secret") {
		t.Fatalf("sensitive value persisted: excerpt=%q normalized=%q", e.Excerpt, e.Normalized)
	}
}

func TestHookVerbosePrintsCaptureResult(t *testing.T) {
	root := t.TempDir()
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))
	var out bytes.Buffer
	if err := Execute([]string{"hook", "claude", "--verbose"}, strings.NewReader(`{"prompt":"Use bun, not npm."}`), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), "hook captured") {
		t.Fatalf("verbose hook output = %q", out.String())
	}
}

func TestConnectAndDisconnectCLIApplyOnly(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("AGBOX_HOME", filepath.Join(home, ".agbox"))
	cmd := fakeAgbox(t, home)
	configPath := filepath.Join(home, ".codex", "hooks.json")

	var out bytes.Buffer
	if err := Execute([]string{"connect", "codex", "--command", cmd}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(configPath); !os.IsNotExist(err) {
		t.Fatalf("connect dry-run wrote config: %v", err)
	}
	if !strings.Contains(out.String(), `"action": "connect"`) {
		t.Fatalf("dry-run output = %s", out.String())
	}

	out.Reset()
	if err := Execute([]string{"connect", "codex", "--apply", "--command", cmd}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(configPath)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(data), "AGBOX_MANAGED_HOOK=1") {
		t.Fatalf("applied config missing managed marker: %s", string(data))
	}

	out.Reset()
	if err := Execute([]string{"disconnect", "codex", "--apply"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	data, err = os.ReadFile(configPath)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(data), "AGBOX_MANAGED_HOOK=1") {
		t.Fatalf("disconnect left managed marker: %s", string(data))
	}
}

func TestConnectAllAndDisconnectAllCLI(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("AGBOX_HOME", filepath.Join(home, ".agbox"))
	cmd := fakeAgbox(t, home)
	codexConfig := filepath.Join(home, ".codex", "hooks.json")
	claudeConfig := filepath.Join(home, ".claude", "settings.json")

	var out bytes.Buffer
	if err := Execute([]string{"connect", "all", "--command", cmd}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(codexConfig); !os.IsNotExist(err) {
		t.Fatalf("connect all dry-run wrote codex config: %v", err)
	}
	if _, err := os.Stat(claudeConfig); !os.IsNotExist(err) {
		t.Fatalf("connect all dry-run wrote claude config: %v", err)
	}
	if !strings.Contains(out.String(), `"agent": "codex"`) || !strings.Contains(out.String(), `"agent": "claude"`) {
		t.Fatalf("connect all plan = %s", out.String())
	}

	out.Reset()
	if err := Execute([]string{"connect", "all", "--apply", "--command", cmd}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	for _, path := range []string{codexConfig, claudeConfig} {
		data, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		if !strings.Contains(string(data), "AGBOX_MANAGED_HOOK=1") {
			t.Fatalf("applied config %s missing managed marker: %s", path, string(data))
		}
	}

	out.Reset()
	if err := Execute([]string{"disconnect", "all", "--apply"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	for _, path := range []string{codexConfig, claudeConfig} {
		data, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		if strings.Contains(string(data), "AGBOX_MANAGED_HOOK=1") {
			t.Fatalf("disconnect all left managed marker in %s: %s", path, string(data))
		}
	}
}

func TestConnectAllPreflightsBeforeWritingAnyConfig(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("AGBOX_HOME", filepath.Join(home, ".agbox"))
	cmd := fakeAgbox(t, home)
	codexConfig := filepath.Join(home, ".codex", "hooks.json")
	claudeConfig := filepath.Join(home, ".claude", "settings.json")
	if err := os.MkdirAll(filepath.Dir(claudeConfig), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(filepath.Join(home, "real-settings.json"), claudeConfig); err != nil {
		t.Fatal(err)
	}

	err := Execute([]string{"connect", "all", "--apply", "--command", cmd}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{})
	if err == nil {
		t.Fatal("connect all succeeded with symlinked claude config")
	}
	if _, err := os.Stat(codexConfig); !os.IsNotExist(err) {
		t.Fatalf("connect all wrote codex config before claude preflight failed: %v", err)
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
		"Hook status",
		"agbox connect all --apply",
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
		"agbox discover",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("demo output missing %q:\n%s", want, got)
		}
	}
	if _, err := os.Stat(dbPath); !os.IsNotExist(err) {
		t.Fatalf("demo touched persistent AGBOX_DB: %v", err)
	}
}

func fakeAgbox(t *testing.T, dir string) string {
	t.Helper()
	path := filepath.Join(dir, "agbox")
	if err := os.WriteFile(path, []byte("#!/bin/sh\nexit 0\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	return path
}
