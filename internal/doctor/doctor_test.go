package doctor

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	_ "github.com/hippoom/agbox/internal/session/claude"
	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/telemetry"
)

func TestRunReportsStoreWatcherAndSources(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("AGBOX_DB", filepath.Join(home, "agbox.db"))

	s, err := store.Open("")
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	report := Run(s)
	if !report.OK {
		t.Fatalf("report should be OK: %#v", report.Lines)
	}
	for _, needle := range []string{
		"store: OK",
		"corrections: 0",
		"telemetry: on (not configured",
		"watcher:",
		"last sync: never",
		"source claude:",
	} {
		if !reportContains(report, needle) {
			t.Fatalf("report missing %q: %#v", needle, report.Lines)
		}
	}
}

func TestRunReportsTelemetryOffAfterOptOut(t *testing.T) {
	home := filepath.Join(t.TempDir(), ".agbox")
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("AGBOX_HOME", home)
	t.Setenv("AGBOX_DB", filepath.Join(home, "agbox.db"))

	if err := telemetry.OptOut(); err != nil {
		t.Fatal(err)
	}

	s, err := store.Open("")
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	report := Run(s)
	if !reportContains(report, "telemetry: off") {
		t.Fatalf("report missing telemetry off: %#v", report.Lines)
	}
}

func reportContains(report Report, needle string) bool {
	for _, line := range report.Lines {
		if strings.Contains(line, needle) {
			return true
		}
	}
	return false
}