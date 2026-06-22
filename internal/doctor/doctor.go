package doctor

import (
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/watcher"
)

type Report struct {
	Lines []string
	OK    bool
}

func Run(s *store.Store) Report {
	stats, err := s.Stats()
	r := Report{OK: err == nil}
	if err != nil {
		r.Lines = append(r.Lines, "store: FAIL "+err.Error())
		return r
	}
	r.Lines = append(r.Lines, "store: OK "+stats.Path)
	r.Lines = append(r.Lines, fmt.Sprintf("events: %d", stats.Events))
	corrections, err := s.CountCorrections()
	if err != nil {
		r.Lines = append(r.Lines, "corrections: FAIL "+err.Error())
		r.OK = false
	} else {
		r.Lines = append(r.Lines, fmt.Sprintf("corrections: %d", corrections))
	}
	r.Lines = append(r.Lines, fmt.Sprintf("candidates: %d", stats.Candidates))
	r.Lines = append(r.Lines, fmt.Sprintf("exports: %d", stats.Exports))

	home, err := os.UserHomeDir()
	if err != nil {
		r.Lines = append(r.Lines, "watcher: unknown ("+err.Error()+")")
	} else {
		ws := watcher.Status(home)
		r.Lines = append(r.Lines, "watcher: "+watcherState(ws))
	}

	lastSync, err := s.LatestCursorSync()
	if err != nil {
		r.Lines = append(r.Lines, "last sync: FAIL "+err.Error())
		r.OK = false
	} else if lastSync.IsZero() {
		r.Lines = append(r.Lines, "last sync: never")
	} else {
		r.Lines = append(r.Lines, "last sync: "+formatLastSync(lastSync))
	}

	for _, adapter := range session.All() {
		sources, err := adapter.DiscoverSources()
		line := fmt.Sprintf("source %s: %d paths", adapter.Agent(), len(sources))
		if err != nil {
			line += " (" + err.Error() + ")"
			r.OK = false
		}
		r.Lines = append(r.Lines, line)
	}
	return r
}

func watcherState(ws watcher.WatcherStatus) string {
	if ws.Running {
		if ws.PID > 0 {
			return fmt.Sprintf("running (pid %d)", ws.PID)
		}
		return "running"
	}
	if ws.Installed {
		return "installed (not running)"
	}
	return "not installed"
}

func formatLastSync(t time.Time) string {
	age := time.Since(t)
	switch {
	case age < time.Minute:
		return "just now"
	case age < time.Hour:
		return fmt.Sprintf("%dm ago", int(age.Minutes()))
	case age < 24*time.Hour:
		return fmt.Sprintf("%dh ago", int(age.Hours()))
	default:
		return t.Format(time.RFC3339)
	}
}

func DebugBundle(s *store.Store, out string) (string, error) {
	if out == "" {
		out = "agbox-debug-bundle.txt"
	}
	report := Run(s)
	if err := os.MkdirAll(filepath.Dir(out), 0o755); filepath.Dir(out) != "." && err != nil {
		return "", err
	}
	f, err := os.Create(out)
	if err != nil {
		return "", err
	}
	defer f.Close()
	for _, line := range report.Lines {
		fmt.Fprintln(f, line)
	}
	fmt.Fprintln(f, "note: bundle is sanitized; no raw prompt text is included")
	return out, nil
}