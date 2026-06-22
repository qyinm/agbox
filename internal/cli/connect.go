package cli

import (
	"flag"
	"fmt"
	"io"
	"os"
	"strings"

	"github.com/hippoom/agbox/internal/connect"
)

func runConnect(args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("connect", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	command := fs.String("command", "", "absolute path to agbox binary")
	project := fs.Bool("project", false, "install project-scoped hooks (grok only)")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"command": true, "project": true})); err != nil {
		return err
	}
	if len(fs.Args()) == 0 {
		return fmt.Errorf("usage: agbox connect <claude|codex|grok> [--command path] [--project]")
	}
	return applyConnect(fs.Args()[0], connect.ActionConnect, connect.Options{
		Command: *command,
		Project: *project,
	}, stdout)
}

func runDisconnect(args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("disconnect", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	project := fs.Bool("project", false, "remove project-scoped hooks (grok only)")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"project": true})); err != nil {
		return err
	}
	if len(fs.Args()) == 0 {
		return fmt.Errorf("usage: agbox disconnect <claude|codex|grok> [--project]")
	}
	return applyConnect(fs.Args()[0], connect.ActionDisconnect, connect.Options{Project: *project}, stdout)
}

func applyConnect(agent, action string, opts connect.Options, stdout io.Writer) error {
	plan, err := connect.BuildPlan(agent, action, opts)
	if err != nil {
		return err
	}
	result, err := connect.Apply(plan)
	if err != nil {
		return err
	}
	if result.Changed {
		fmt.Fprintf(stdout, "%s %s: updated %s\n", action, agent, plan.Path)
		if result.BackupPath != "" {
			fmt.Fprintf(stdout, "backup: %s\n", result.BackupPath)
		}
	} else {
		fmt.Fprintf(stdout, "%s %s: no changes needed (%s)\n", action, agent, plan.Path)
	}
	if plan.UnsupportedTOML != "" {
		fmt.Fprintf(stdout, "warning: codex hooks in %s must be consolidated into ~/.codex/hooks.json manually\n", plan.UnsupportedTOML)
	}
	return nil
}

func connectAllAgents(stdout io.Writer) error {
	agents := []struct {
		agent string
		skip  string
		note  string
	}{
		{connect.AgentClaude, "AGBOX_SKIP_CONNECT_CLAUDE", ""},
		{connect.AgentCodex, "AGBOX_SKIP_CONNECT_CODEX", "  → Codex: run /hooks and trust agbox hooks"},
		{connect.AgentGrok, "AGBOX_SKIP_CONNECT_GROK", "  → Grok:  user-global hooks at ~/.grok/hooks/agbox.json"},
	}
	var connected []string
	for _, item := range agents {
		if isTruthyEnv(item.skip) {
			continue
		}
		if err := applyConnect(item.agent, connect.ActionConnect, connect.Options{}, io.Discard); err != nil {
			fmt.Fprintf(stdout, "agbox: connect %s skipped (%v)\n", item.agent, err)
			continue
		}
		connected = append(connected, item.agent)
		if item.note != "" {
			fmt.Fprintln(stdout, item.note)
		}
	}
	if len(connected) > 0 {
		fmt.Fprintf(stdout, "agbox: hooks installed for %s\n", strings.Join(connected, ", "))
	}
	return nil
}

func isTruthyEnv(key string) bool {
	v := strings.ToLower(strings.TrimSpace(os.Getenv(key)))
	return v == "1" || v == "true" || v == "yes"
}