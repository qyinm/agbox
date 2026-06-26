package cli

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/hippoom/agbox/internal/propose"
	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/tui"
)

var (
	interactiveTerminalHook = interactiveTerminal
	launchWorkspaceProgram  = defaultLaunchWorkspaceProgram
)

func maybeRunWorkspace(args []string, stdin io.Reader, stdout io.Writer) (bool, error) {
	if !interactiveTerminalHook(stdin) || !interactiveTerminalHook(stdout) {
		return false, nil
	}
	if len(args) > 0 {
		if args[0] == "-h" || args[0] == "--help" || hasHelpFlag(args[1:]) {
			return false, nil
		}
	}
	opts, needsStore, ok, err := workspaceOptions(args)
	if err != nil {
		return true, err
	}
	if !ok {
		return false, nil
	}
	if !needsStore {
		return true, launchWorkspaceProgram(opts, stdin, stdout)
	}
	return true, withStore(func(s *store.Store) error {
		opts.Store = s
		if opts.InitialScreen == tui.WorkspaceStatus {
			result, err := propose.ReconcileAcceptedSkills(s)
			if err != nil {
				return err
			}
			opts.AcceptedSkillsReconciled = result.Accepted
		}
		return launchWorkspaceProgram(opts, stdin, stdout)
	})
}

func workspaceOptions(args []string) (tui.WorkspaceOptions, bool, bool, error) {
	opts := tui.WorkspaceOptions{
		InitialScreen: tui.WorkspaceOverview,
		CommandHelp:   commandHelp,
	}
	if len(args) == 0 {
		return opts, true, true, nil
	}
	switch args[0] {
	case "help":
		opts.InitialScreen = tui.WorkspaceHelp
		if len(args) > 1 {
			command := strings.ToLower(strings.TrimSpace(args[1]))
			if _, ok := commandHelp[command]; !ok {
				return opts, false, true, fmt.Errorf("unknown command %q", args[1])
			}
			opts.HelpCommand = command
		}
		return opts, false, true, nil
	case "status":
		opts.InitialScreen = tui.WorkspaceStatus
	case "sources":
		opts.InitialScreen = tui.WorkspaceSources
	case "doctor", "repair":
		opts.InitialScreen = tui.WorkspaceRepair
	case "inbox":
		state, err := parseInboxState(args[1:])
		if err != nil {
			return opts, true, true, err
		}
		opts.InitialScreen = tui.WorkspaceWorkflows
		opts.WorkflowState = state
	case "review":
		reviewOpts, err := parseReviewOptions(args[1:])
		if err != nil {
			return opts, true, true, err
		}
		opts.InitialScreen = tui.WorkspaceReview
		opts.ReviewOptions = reviewOpts
	case "evidence":
		if len(args) == 0 || len(args[1:]) == 0 {
			return opts, true, true, errors.New("usage: agbox evidence <candidate-id>")
		}
		opts.InitialScreen = tui.WorkspaceEvidence
		opts.EvidenceID = args[1]
	default:
		return opts, false, false, nil
	}
	return opts, true, true, nil
}

func defaultLaunchWorkspaceProgram(opts tui.WorkspaceOptions, stdin io.Reader, stdout io.Writer) error {
	m := tui.NewWorkspaceModel(opts)
	_, err := tea.NewProgram(m, tea.WithInput(stdin), tea.WithOutput(stdout)).Run()
	if errors.Is(err, tea.ErrInterrupted) {
		return nil
	}
	return err
}

func stripPlainFlag(args []string) ([]string, bool) {
	out := make([]string, 0, len(args))
	plain := false
	passthrough := false
	for _, arg := range args {
		if passthrough {
			out = append(out, arg)
			continue
		}
		if arg == "--" {
			passthrough = true
			out = append(out, arg)
			continue
		}
		switch arg {
		case "--plain", "--no-tui":
			plain = true
			continue
		default:
			out = append(out, arg)
		}
	}
	return out, plain
}

func stripWorkspacePlainFlag(args []string) ([]string, bool) {
	stripped, plain := stripPlainFlag(args)
	if !plain {
		return args, false
	}
	if workspacePlainCommand(stripped) {
		return stripped, true
	}
	return args, false
}

func workspacePlainCommand(args []string) bool {
	if len(args) == 0 {
		return true
	}
	switch args[0] {
	case "help", "status", "sources", "doctor", "repair", "inbox", "review", "evidence":
		return true
	default:
		return false
	}
}

func parseReviewOptions(args []string) (tui.ReviewOptions, error) {
	fs := flag.NewFlagSet("review", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	state := fs.String("state", stringDefaultReviewState(), "candidate state filter, or all")
	minRepeats := fs.Int("min-repeats", 2, "minimum repeated signals")
	limit := fs.Int("limit", 20, "maximum workflows to show")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"state": true, "min-repeats": true, "limit": true})); err != nil {
		return tui.ReviewOptions{}, err
	}
	if *limit < 0 {
		return tui.ReviewOptions{}, errors.New("--limit must be 0 or greater")
	}
	if !validReviewState(*state) {
		return tui.ReviewOptions{}, fmt.Errorf("--state must be %s", reviewStateHelp)
	}
	return tui.ReviewOptions{
		State:      *state,
		MinRepeats: *minRepeats,
		Limit:      *limit,
		Project:    defaultProject(),
	}, nil
}

func parseInboxState(args []string) (string, error) {
	fs := flag.NewFlagSet("inbox", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	state := fs.String("state", "", "recorded workflow state filter, or all")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"state": true})); err != nil {
		return "", err
	}
	return normalizeCandidateStateFilter(*state)
}

func stringDefaultReviewState() string {
	return "pending"
}

func normalizeCandidateStateFilter(state string) (string, error) {
	stateFilter := strings.ToLower(strings.TrimSpace(state))
	if stateFilter == "all" {
		stateFilter = ""
	}
	if stateFilter != "" && !validReviewState(stateFilter) {
		return "", fmt.Errorf("--state must be %s", reviewStateHelp)
	}
	return stateFilter, nil
}
