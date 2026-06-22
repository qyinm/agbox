package cli

import (
	"flag"
	"fmt"
	"io"
	"os"
	"time"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/watcher"
)

func runInit(args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("init", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	quiet := fs.Bool("quiet", false, "suppress status output")
	if err := fs.Parse(reorderFlags(args, map[string]bool{})); err != nil {
		return err
	}

	s, err := store.Open("")
	if err != nil {
		return err
	}
	defer s.Close()
	if err := os.MkdirAll(".agbox", 0o755); err != nil {
		return err
	}
	if err := ensureGitignore(".agbox/"); err != nil {
		return err
	}

	home := os.Getenv("HOME")
	if home == "" {
		home, err = os.UserHomeDir()
		if err != nil {
			return err
		}
	}
	agboxBin, err := os.Executable()
	if err != nil {
		return err
	}
	if err := watcher.Install(home, agboxBin); err != nil {
		return err
	}

	ingested, err := session.IngestAll(s)
	if err != nil {
		return err
	}

	if *quiet {
		return nil
	}

	fmt.Fprintf(stdout, `Initialized agbox
store: %s
project: .agbox/

`, s.Path())
	printWatcherStatus(stdout, home)
	fmt.Fprintf(stdout, "initial ingest: %d corrections\n\n", ingested)
	fmt.Fprintln(stdout, `Next steps:
  agbox review            # Review workflow candidates
  agbox status            # Check watcher and sync status
  agbox demo              # See the workflow in action`)
	return nil
}

func printWatcherStatus(w io.Writer, home string) {
	status := watcher.Status(home)
	state := "stopped"
	if status.Running {
		if status.PID > 0 {
			state = fmt.Sprintf("running (pid %d)", status.PID)
		} else {
			state = "running"
		}
	} else if status.Installed {
		state = "installed (not running)"
	} else {
		state = "not installed"
	}
	fmt.Fprintf(w, "watcher: %s\n", state)
	fmt.Fprintf(w, "plist: %s\n", status.PlistPath)
}

func formatLastSync(t time.Time) string {
	if t.IsZero() {
		return "never"
	}
	ago := time.Since(t).Round(time.Second)
	if ago < time.Minute {
		return fmt.Sprintf("%s ago", ago)
	}
	if ago < time.Hour {
		return fmt.Sprintf("%s ago", ago.Round(time.Minute))
	}
	return fmt.Sprintf("%s ago", ago.Round(time.Hour))
}