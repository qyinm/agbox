package doctor

import (
	"fmt"
	"os"
	"path/filepath"

	hookconnect "github.com/hippoom/agbox/internal/connect"
	"github.com/hippoom/agbox/internal/store"
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
	r.Lines = append(r.Lines, fmt.Sprintf("candidates: %d", stats.Candidates))
	r.Lines = append(r.Lines, fmt.Sprintf("exports: %d", stats.Exports))
	for _, status := range hookconnect.StatusAll() {
		line := fmt.Sprintf("hook %s: %s", status.Agent, status.State)
		if status.Path != "" {
			line += " " + status.Path
		}
		if status.Command != "" {
			line += " command=" + status.Command
		}
		if status.Detail != "" {
			line += " (" + status.Detail + ")"
		}
		r.Lines = append(r.Lines, line)
		if !status.OK {
			r.OK = false
		}
	}
	return r
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
