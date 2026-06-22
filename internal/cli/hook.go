package cli

import (
	"flag"
	"fmt"
	"io"

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
	fs := flag.NewFlagSet("hook propose", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	_ = fs.String("event", "", "session-start|stop")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"event": true})); err != nil {
		return err
	}
	if len(fs.Args()) == 0 {
		return fmt.Errorf("usage: agbox hook propose <claude|codex|grok> [--event session-start|stop]")
	}
	agent := fs.Args()[0]
	hookData, _ := io.ReadAll(stdin)
	if err := pipeline.SyncIfStale(s); err != nil {
		return err
	}
	project := propose.ProjectFromHook(hookData)
	if project == "" {
		project = defaultProject()
	}
	payload, err := propose.Propose(s, agent, project)
	if err != nil {
		return err
	}
	if payload == "" {
		return nil
	}
	_, err = io.WriteString(stdout, payload)
	return err
}

func runHookAcknowledge(s *store.Store, args []string, stdin io.Reader, stdout io.Writer) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: agbox hook acknowledge <claude|codex|grok>")
	}
	agent := args[0]
	hookData, _ := io.ReadAll(stdin)
	return propose.Acknowledge(s, agent, hookData)
}