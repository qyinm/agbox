package cli

import (
	"fmt"
	"io"
	"os"

	"github.com/hippoom/agbox/internal/pipeline"
	"github.com/hippoom/agbox/internal/propose"
	"github.com/hippoom/agbox/internal/store"
)

func runHook(s *store.Store, args []string, stdin io.Reader, stdout io.Writer) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: agbox hook propose|acknowledge <agent>")
	}
	switch args[0] {
	case "propose":
		return runHookPropose(s, args[1:], stdin, stdout)
	case "acknowledge":
		return runHookAcknowledge(s, args[1:], stdin, stdout)
	default:
		return fmt.Errorf("unknown hook subcommand %q", args[0])
	}
}

func runHookPropose(s *store.Store, args []string, stdin io.Reader, stdout io.Writer) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: agbox hook propose <claude|codex|grok>")
	}
	agent := args[0]
	hookData, err := io.ReadAll(stdin)
	if err != nil {
		return err
	}
	syncResult, err := pipeline.SyncBestEffortIfStale(s)
	if err != nil {
		return err
	}
	if syncResult.Warning != nil {
		fmt.Fprintf(os.Stderr, "agbox: warning: partial sync before proposal: %s\n", syncResult.Warning)
	}
	project := propose.ProjectFromHook(hookData)
	if project == "" {
		project = defaultProject()
	}
	candidateID, payload, err := propose.SelectAndRender(s, agent, project)
	if err != nil {
		return err
	}
	if payload == "" {
		return nil
	}
	return propose.DeliverProposed(s, candidateID, payload, stdout, os.Stderr)
}

func runHookAcknowledge(s *store.Store, args []string, stdin io.Reader, stdout io.Writer) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: agbox hook acknowledge <claude|codex|grok>")
	}
	agent := args[0]
	hookData, err := io.ReadAll(stdin)
	if err != nil {
		return err
	}
	return propose.Acknowledge(s, agent, hookData)
}
