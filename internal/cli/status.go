package cli

import (
	"fmt"
	"io"

	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/watcher"
)

func runStatus(s *store.Store, stdout io.Writer) error {
	home, err := userHome()
	if err != nil {
		return err
	}
	ws := watcher.Status(home)
	state := "stopped"
	if ws.Running {
		if ws.PID > 0 {
			state = fmt.Sprintf("running (pid %d)", ws.PID)
		} else {
			state = "running"
		}
	} else if ws.Installed {
		state = "installed (not running)"
	}

	stats, err := s.Stats()
	if err != nil {
		return err
	}
	corrections, err := s.CountCorrections()
	if err != nil {
		return err
	}
	lastSync, err := s.LatestCursorSync()
	if err != nil {
		return err
	}

	fmt.Fprintf(stdout, "watcher: %s\n", state)
	fmt.Fprintf(stdout, "store: %s\n", stats.Path)
	fmt.Fprintf(stdout, "last sync: %s\n", formatLastSync(lastSync))
	fmt.Fprintf(stdout, "corrections: %d\n", corrections)
	fmt.Fprintf(stdout, "candidates: %d\n", stats.Candidates)
	fmt.Fprintf(stdout, "events: %d\n", stats.Events)
	fmt.Fprintf(stdout, "exports: %d\n", stats.Exports)
	return nil
}