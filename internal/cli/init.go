package cli

import (
	"flag"
	"fmt"
	"io"
	"os"
	"time"

	"github.com/hippoom/agbox/internal/pipeline"
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

	syncResult, syncFatalErr := pipeline.SyncBestEffort(s)
	if syncFatalErr != nil {
		return syncFatalErr
	}
	connectOut := stdout
	if *quiet {
		connectOut = io.Discard
	}
	if err := connectAllAgents(connectOut); err != nil {
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
	fmt.Fprintf(stdout, "managed hooks: %s\n", managedHookSummary())
	fmt.Fprintln(stdout, "telemetry: on by default (agbox telemetry off to opt out)")
	if syncResult.Warning != nil {
		fmt.Fprintf(stdout, "initial ingest: partial (%d corrections; run agbox doctor if candidates look wrong)\n\n", syncResult.Ingested)
	} else {
		fmt.Fprintf(stdout, "initial ingest: %d corrections\n\n", syncResult.Ingested)
	}
	fmt.Fprintln(stdout, `Next steps:
  agbox beta              # See setup + candidates in one terminal summary
  agbox doctor            # Check watcher + managed proposal hooks
  agbox status            # Check watcher and sync status
  agbox demo              # See the workflow in action
  agbox disconnect <agent> # Remove managed proposal hooks`)
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
