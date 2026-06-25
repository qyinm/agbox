package cli

import (
	"flag"
	"fmt"
	"io"
	"strings"

	"github.com/hippoom/agbox/internal/evidence"
	"github.com/hippoom/agbox/internal/pipeline"
	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/workflow"
)

func runInbox(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("inbox", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	state := fs.String("state", "", "recorded workflow state filter, or all")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"state": true})); err != nil {
		return err
	}
	syncResult, err := pipeline.SyncBestEffortIfStale(s)
	if err != nil {
		return err
	}
	if syncResult.Warning != nil {
		fmt.Fprintf(stdout, "warning: partial sync before inbox: %s\n\n", syncResult.Warning)
	}
	stateFilter := strings.ToLower(strings.TrimSpace(*state))
	if stateFilter == "all" {
		stateFilter = ""
	}
	if stateFilter != "" && !validReviewState(stateFilter) {
		return fmt.Errorf("--state must be %s", reviewStateHelp)
	}
	candidates, err := s.ListCandidates(stateFilter)
	if err != nil {
		return err
	}
	if len(candidates) == 0 {
		fmt.Fprintln(stdout, "No Recorded Workflows yet.")
		fmt.Fprintln(stdout, "Keep using your agents; agbox records repeated prompts and corrections automatically.")
		fmt.Fprintln(stdout, "Try the loop without touching your data: agbox demo")
		fmt.Fprintln(stdout, "Check setup anytime: agbox doctor")
		return nil
	}
	fmt.Fprintf(stdout, "Recorded Workflows (showing %d %s)\n", len(candidates), displayState(stateFilter))
	for i, c := range candidates {
		evidenceCard, err := evidence.Build(s, c.ID)
		if err != nil {
			return err
		}
		card := workflow.Build(evidenceCard)
		fmt.Fprintf(stdout, "\n%d. %s (%s)\n", i+1, card.Name, c.ID)
		fmt.Fprintf(stdout, "   lifecycle=%s confidence=%s repeats=%d projects=%d source=%s\n",
			card.Lifecycle, c.Confidence, c.EventCount, c.ProjectCount, c.SourceKind)
		fmt.Fprintf(stdout, "   when: %s\n", card.WhenItApplies)
		fmt.Fprintln(stdout, "   replay:")
		for stepIndex, step := range card.ReplayPlan {
			fmt.Fprintf(stdout, "     %d. %s\n", stepIndex+1, step)
		}
		fmt.Fprintf(stdout, "   evidence: %s\n", card.EvidenceSummary)
		fmt.Fprintf(stdout, "   safety: %s\n", card.SafetyNote)
		fmt.Fprintf(stdout, "   actions: agbox evidence %s | agbox reject %s | agbox snooze %s\n", c.ID, c.ID, c.ID)
	}
	return nil
}
