package cli

import (
	"flag"
	"fmt"
	"io"
	"strings"

	"github.com/hippoom/agbox/internal/connect"
	"github.com/hippoom/agbox/internal/evidence"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/pipeline"
	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/watcher"
)

var betaStatePriority = []model.CandidateState{
	model.CandidateProposalReady,
	model.CandidateProposed,
	model.CandidatePending,
	model.CandidateApproved,
}

func runBeta(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("beta", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	limit := fs.Int("limit", 5, "maximum candidates to show")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"limit": true})); err != nil {
		return err
	}
	if *limit < 0 {
		return fmt.Errorf("--limit must be 0 or greater")
	}

	syncResult, syncFatalErr := pipeline.SyncBestEffort(s)
	if syncFatalErr != nil {
		return syncFatalErr
	}
	ingested := syncResult.Ingested
	syncErr := syncResult.Warning
	stats, statsErr := s.Stats()
	if statsErr != nil {
		return statsErr
	}
	corrections, correctionsErr := s.CountCorrections()
	if correctionsErr != nil {
		return correctionsErr
	}
	lastSync, lastSyncErr := s.LatestCursorSync()
	if lastSyncErr != nil {
		return lastSyncErr
	}

	fmt.Fprintln(stdout, "agbox beta")
	fmt.Fprintln(stdout)
	fmt.Fprintln(stdout, "Setup")
	fmt.Fprintf(stdout, "  watcher: %s\n", betaWatcherState())
	fmt.Fprintf(stdout, "  managed hooks: %s\n", managedHookSummary())
	fmt.Fprintf(stdout, "  sources: %s\n", betaSourceState())
	fmt.Fprintf(stdout, "  store: %s\n", stats.Path)
	fmt.Fprintf(stdout, "  last sync: %s\n", formatLastSync(lastSync))
	fmt.Fprintf(stdout, "  corrections: %d\n", corrections)
	fmt.Fprintf(stdout, "  prompt events: %d\n", stats.Events)
	fmt.Fprintf(stdout, "  candidates: %d\n", stats.Candidates)
	if syncErr != nil {
		fmt.Fprintf(stdout, "  sync: partial (%s)\n", betaSyncIssue(syncErr))
	} else {
		fmt.Fprintf(stdout, "  sync: ok (%d new corrections)\n", ingested)
	}
	if *limit == 0 {
		fmt.Fprintln(stdout)
		fmt.Fprintln(stdout, "Candidate display disabled by --limit 0.")
		fmt.Fprintln(stdout, "Run agbox beta --limit 5 to show candidates.")
		return nil
	}

	candidates, err := betaCandidates(s, *limit)
	if err != nil {
		return err
	}
	if len(candidates) == 0 {
		fmt.Fprintln(stdout)
		fmt.Fprintln(stdout, "No repeated corrections yet.")
		fmt.Fprintln(stdout, "No repeated prompt patterns yet.")
		fmt.Fprintln(stdout, "Keep working in Claude, Codex, Cursor, or Grok; agbox will watch for repeated workflow signals.")
		fmt.Fprintln(stdout, "Try the loop without touching your data: agbox demo")
		fmt.Fprintln(stdout, "Check setup anytime: agbox doctor")
		return nil
	}

	fmt.Fprintln(stdout)
	fmt.Fprintf(stdout, "Workflow candidates (showing %d)\n", len(candidates))
	for i, c := range candidates {
		card, err := evidence.Build(s, c.ID)
		if err != nil {
			return err
		}
		fmt.Fprintf(stdout, "\n%d. %s (%s)\n", i+1, c.Name, c.ID)
		fmt.Fprintf(stdout, "   state=%s source=%s confidence=%s repeats=%d projects=%d\n", c.State, c.SourceKind, c.Confidence, c.EventCount, c.ProjectCount)
		if example := betaEvidenceExample(card); example != "" {
			fmt.Fprintf(stdout, "   example: %s\n", example)
		}
		fmt.Fprintf(stdout, "   next: %s\n", betaNextAction(c))
		fmt.Fprintf(stdout, "   inspect: agbox evidence %s\n", c.ID)
	}
	return nil
}

func betaCandidates(s *store.Store, limit int) ([]model.Candidate, error) {
	if limit == 0 {
		return nil, nil
	}
	seen := map[string]bool{}
	out := make([]model.Candidate, 0, limit)
	for _, state := range betaStatePriority {
		candidates, err := s.ListCandidatesByState(state)
		if err != nil {
			return nil, err
		}
		for _, c := range candidates {
			if seen[c.ID] {
				continue
			}
			seen[c.ID] = true
			out = append(out, c)
			if len(out) == limit {
				return out, nil
			}
		}
	}
	return out, nil
}

func betaEvidenceExample(card model.EvidenceCard) string {
	if len(card.Occurrences) > 0 {
		return strings.ReplaceAll(card.Occurrences[0].SummaryLine(), "  →  ", " -> ")
	}
	if len(card.Excerpts) > 0 {
		return card.Excerpts[0]
	}
	return strings.TrimSpace(card.Candidate.RuleText)
}

func betaNextAction(c model.Candidate) string {
	switch c.State {
	case model.CandidateProposalReady:
		if c.SourceKind == model.CandidateSourcePromptPattern {
			return "ready to propose a recurring-prompt skill inside your agent; keep working or run agbox review --state proposal_ready"
		}
		return "ready to propose inside your agent; keep working or run agbox review --state proposal_ready"
	case model.CandidateProposed:
		return "answer the in-agent proposal, or run agbox snooze " + c.ID
	case model.CandidateApproved:
		return "preview a safe write with agbox export " + c.ID + " --target agents-md --dry-run"
	default:
		return "review with agbox evidence " + c.ID + " or agbox review"
	}
}

func betaWatcherState() string {
	home, err := userHome()
	if err != nil {
		return "unknown (" + err.Error() + ")"
	}
	ws := watcher.Status(home)
	if ws.Running {
		if ws.PID > 0 {
			return fmt.Sprintf("running (pid %d)", ws.PID)
		}
		return "running"
	}
	if ws.Installed {
		return "installed (not running)"
	}
	return "not installed"
}

func managedHookSummary() string {
	statuses := connect.StatusAll()
	connected := 0
	var needs []string
	for _, st := range statuses {
		if st.State == "connected" {
			connected++
			continue
		}
		if !st.OK {
			needs = append(needs, st.Agent)
		}
	}
	summary := fmt.Sprintf("%d/%d connected", connected, len(statuses))
	if len(needs) > 0 {
		summary += " (" + strings.Join(needs, ", ") + " need attention)"
	}
	return summary
}

func betaSourceState() string {
	count := 0
	var failed []string
	for _, adapter := range session.All() {
		sources, err := adapter.DiscoverSources()
		if err != nil {
			failed = append(failed, adapter.Agent())
			continue
		}
		count += len(sources)
	}
	if len(failed) > 0 {
		return fmt.Sprintf("%d discovered (%s failed)", count, strings.Join(failed, ", "))
	}
	return fmt.Sprintf("%d discovered", count)
}

func betaSyncIssue(err error) string {
	msg := err.Error()
	if strings.Contains(msg, "token too long") {
		return "one session file was too large to parse; run agbox doctor if candidates look wrong"
	}
	return "run agbox doctor if candidates look wrong"
}
