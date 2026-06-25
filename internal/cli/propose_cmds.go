package cli

import (
	"flag"
	"fmt"
	"io"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/propose"
	"github.com/hippoom/agbox/internal/store"
)

func runReject(s *store.Store, args []string, stdout io.Writer) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: agbox reject <candidate-id>")
	}
	if err := propose.Reject(s, args[0]); err != nil {
		return err
	}
	fmt.Fprintf(stdout, "%s -> rejected (7d cooldown)\n", args[0])
	return nil
}

func runSnooze(s *store.Store, args []string, stdout io.Writer) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: agbox snooze <candidate-id>")
	}
	if err := propose.Snooze(s, args[0]); err != nil {
		return err
	}
	fmt.Fprintf(stdout, "%s -> snoozed (24h)\n", args[0])
	return nil
}

func runAccept(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("accept", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	skillPath := fs.String("skill-path", "", "path to created SKILL.md")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"skill-path": true})); err != nil {
		return err
	}
	if len(fs.Args()) == 0 {
		return fmt.Errorf("usage: agbox accept <candidate-id> [--skill-path path]")
	}
	if err := propose.Accept(s, fs.Args()[0], *skillPath); err != nil {
		return err
	}
	fmt.Fprintf(stdout, "%s -> accepted\n", fs.Args()[0])
	return nil
}

func runApply(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("apply", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	agent := fs.String("agent", "", "agent that applied the replay")
	project := fs.String("project", "", "project where replay was applied")
	promptHash := fs.String("prompt-hash", "", "hash of the matched prompt")
	promptExcerpt := fs.String("prompt-excerpt", "", "redacted excerpt of the matched prompt")
	if err := fs.Parse(reorderFlags(args, map[string]bool{
		"agent":          true,
		"project":        true,
		"prompt-hash":    true,
		"prompt-excerpt": true,
	})); err != nil {
		return err
	}
	if len(fs.Args()) == 0 {
		return fmt.Errorf("usage: agbox apply <candidate-id> [--agent agent] [--project project] [--prompt-hash hash] [--prompt-excerpt text]")
	}
	candidateID := fs.Args()[0]
	if err := propose.ApplyOnce(s, model.ReplayApplication{
		CandidateID:   candidateID,
		Agent:         *agent,
		Project:       *project,
		PromptHash:    *promptHash,
		PromptExcerpt: *promptExcerpt,
	}); err != nil {
		return err
	}
	fmt.Fprintf(stdout, "%s -> applied once\n", candidateID)
	return nil
}
