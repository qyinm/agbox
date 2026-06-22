package doctor

import (
	"path/filepath"
	"strings"
	"testing"

	_ "github.com/hippoom/agbox/internal/session/claude"
	"github.com/hippoom/agbox/internal/store"
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
		"watcher:",
		"last sync: never",
		"source claude:",
	} {
		if !reportContains(report, needle) {
			t.Fatalf("report missing %q: %#v", needle, report.Lines)
		}
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