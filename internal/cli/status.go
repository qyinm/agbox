package cli

import (
	"encoding/json"
	"flag"
	"fmt"
	"io"

	"github.com/hippoom/agbox/internal/propose"
	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/watcher"
)

func runStatus(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("status", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	jsonOutput := fs.Bool("json", false, "write machine-readable status")
	if err := fs.Parse(args); err != nil {
		return err
	}
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

	reconcileResult, err := propose.ReconcileAcceptedSkills(s)
	if err != nil {
		return err
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

	health := s.IngestionHealth()
	if *jsonOutput {
		payload := struct {
			Version      int                   `json:"version"`
			Watcher      string                `json:"watcher"`
			ManagedHooks string                `json:"managed_hooks"`
			Ingestion    store.IngestionHealth `json:"ingestion"`
			LastSync     string                `json:"last_sync"`
			Corrections  int                   `json:"corrections"`
			Workflows    int                   `json:"recorded_workflows"`
			Events       int                   `json:"events"`
			Exports      int                   `json:"exports"`
		}{Version: 1, Watcher: state, ManagedHooks: managedHookSummary(), Ingestion: health,
			LastSync: formatLastSync(lastSync), Corrections: corrections, Workflows: stats.Candidates, Events: stats.Events, Exports: stats.Exports}
		if err := json.NewEncoder(stdout).Encode(payload); err != nil {
			return err
		}
		return ingestionHealthExitError(health)
	}

	fmt.Fprintf(stdout, "watcher: %s\n", state)
	fmt.Fprintf(stdout, "managed hooks: %s\n", managedHookSummary())
	fmt.Fprintln(stdout, "store: available")
	fmt.Fprintf(stdout, "last sync: %s\n", formatLastSync(lastSync))
	fmt.Fprintf(stdout, "corrections: %d\n", corrections)
	fmt.Fprintf(stdout, "recorded workflows: %d\n", stats.Candidates)
	if reconcileResult.Accepted > 0 {
		fmt.Fprintf(stdout, "accepted skills: %d reconciled\n", reconcileResult.Accepted)
	}
	fmt.Fprintf(stdout, "events: %d\n", stats.Events)
	fmt.Fprintf(stdout, "exports: %d\n", stats.Exports)
	for _, line := range health.PlainLines() {
		fmt.Fprintln(stdout, line)
	}
	return ingestionHealthExitError(health)
}

func ingestionHealthExitError(health store.IngestionHealth) error {
	if health.State == store.HealthDegraded || health.State == store.HealthStalled {
		return fmt.Errorf("ingestion is %s; see status details", health.State)
	}
	return nil
}
