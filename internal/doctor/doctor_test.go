package doctor

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	hookconnect "github.com/hippoom/agbox/internal/connect"
	"github.com/hippoom/agbox/internal/store"
)

func TestRunReportsHookHealth(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("AGBOX_HOME", filepath.Join(home, ".agbox"))
	t.Setenv("AGBOX_DB", filepath.Join(home, "agbox.db"))
	cmd := fakeExecutable(t, home)
	plan, err := hookconnect.BuildPlan(hookconnect.AgentCodex, hookconnect.ActionConnect, hookconnect.Options{Command: cmd})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := hookconnect.Apply(plan); err != nil {
		t.Fatal(err)
	}
	s, err := store.Open("")
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	report := Run(s)
	if !report.OK {
		t.Fatalf("report should be OK: %#v", report)
	}
	if !reportContains(report, "hook codex: connected") {
		t.Fatalf("report missing connected status: %#v", report.Lines)
	}

	if err := os.Chmod(cmd, 0o644); err != nil {
		t.Fatal(err)
	}
	report = Run(s)
	if report.OK {
		t.Fatalf("report should fail for unexecutable hook command: %#v", report)
	}
	if !reportContains(report, "hook codex: unexecutable command") {
		t.Fatalf("report missing unexecutable status: %#v", report.Lines)
	}
}

func TestRunReportsHookParseError(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("AGBOX_DB", filepath.Join(home, "agbox.db"))
	path := filepath.Join(home, ".claude", "settings.json")
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(`{"hooks":`), 0o644); err != nil {
		t.Fatal(err)
	}
	s, err := store.Open("")
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	report := Run(s)
	if report.OK {
		t.Fatalf("report should fail for hook parse error: %#v", report)
	}
	if !reportContains(report, "hook claude: parse error") {
		t.Fatalf("report missing parse error: %#v", report.Lines)
	}
}

func fakeExecutable(t *testing.T, dir string) string {
	t.Helper()
	path := filepath.Join(dir, "agbox")
	if err := os.WriteFile(path, []byte("#!/bin/sh\nexit 0\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	return path
}

func reportContains(report Report, needle string) bool {
	for _, line := range report.Lines {
		if strings.Contains(line, needle) {
			return true
		}
	}
	return false
}
