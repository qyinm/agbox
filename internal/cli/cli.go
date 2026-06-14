package cli

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	"github.com/hippoom/agbox/internal/audit"
	"github.com/hippoom/agbox/internal/capture"
	"github.com/hippoom/agbox/internal/compile"
	hookconnect "github.com/hippoom/agbox/internal/connect"
	"github.com/hippoom/agbox/internal/doctor"
	"github.com/hippoom/agbox/internal/evidence"
	agexport "github.com/hippoom/agbox/internal/export"
	"github.com/hippoom/agbox/internal/fsx"
	"github.com/hippoom/agbox/internal/impact"
	"github.com/hippoom/agbox/internal/manifest"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/store"
)

func Execute(args []string, stdin io.Reader, stdout, stderr io.Writer) error {
	if len(args) == 0 {
		printUsage(stdout)
		return nil
	}
	switch args[0] {
	case "help", "-h", "--help":
		printUsage(stdout)
		return nil
	case "init":
		return runInit(args[1:], stdout)
	case "capture":
		return withStore(func(s *store.Store) error { return runCapture(s, args[1:], stdin, stdout) })
	case "hook":
		return withStore(func(s *store.Store) error { return runHook(s, args[1:], stdin, stdout) })
	case "connect":
		return runConnect(args[1:], stdout)
	case "disconnect":
		return runDisconnect(args[1:], stdout)
	case "discover":
		return withStore(func(s *store.Store) error { return runDiscover(s, args[1:], stdout) })
	case "demo":
		return runDemo(stdout)
	case "scan":
		return withStore(func(s *store.Store) error { return runScan(s, args[1:], stdout) })
	case "inbox":
		return withStore(func(s *store.Store) error { return runInbox(s, args[1:], stdout) })
	case "evidence":
		return withStore(func(s *store.Store) error { return runEvidence(s, args[1:], stdout) })
	case "approve":
		return withStore(func(s *store.Store) error { return runState(s, args[1:], model.CandidateApproved, stdout) })
	case "reject":
		return withStore(func(s *store.Store) error { return runState(s, args[1:], model.CandidateRejected, stdout) })
	case "compile":
		return withStore(func(s *store.Store) error { return runCompile(s, args[1:], stdout) })
	case "export":
		return withStore(func(s *store.Store) error { return runExport(s, args[1:], stdout) })
	case "manifest":
		return withStore(func(s *store.Store) error { return runManifest(args[1:], stdout) })
	case "impact":
		return withStore(func(s *store.Store) error { return runImpact(s, args[1:], stdout) })
	case "audit":
		return withStore(func(s *store.Store) error { return runAudit(s, args[1:], stdout) })
	case "doctor":
		return withStore(func(s *store.Store) error { return runDoctor(s, stdout) })
	case "debug-bundle":
		return withStore(func(s *store.Store) error { return runDebugBundle(s, args[1:], stdout) })
	case "repair":
		return withStore(func(s *store.Store) error { return runRepair(s, stdout) })
	default:
		return fmt.Errorf("unknown command %q", args[0])
	}
}

func PrintError(w io.Writer, err error) {
	fmt.Fprintf(w, "agbox: %v\n", err)
}

func withStore(fn func(*store.Store) error) error {
	s, err := store.Open("")
	if err != nil {
		return err
	}
	defer s.Close()
	return fn(s)
}

func runInit(args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("init", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
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
	fmt.Fprintf(stdout, "Initialized agbox\nstore: %s\nproject: .agbox/\n", s.Path())
	return nil
}

func runCapture(s *store.Store, args []string, stdin io.Reader, stdout io.Writer) error {
	fs := flag.NewFlagSet("capture", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	source := fs.String("source", "manual", "capture source")
	agent := fs.String("agent", "unknown", "agent name")
	project := fs.String("project", defaultProject(), "project name")
	raw := fs.Bool("raw", false, "store redacted raw text")
	noExcerpt := fs.Bool("no-excerpt", false, "store hash and metadata only")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"source": true, "agent": true, "project": true})); err != nil {
		return err
	}
	text := strings.TrimSpace(strings.Join(fs.Args(), " "))
	if text == "" {
		data, err := io.ReadAll(stdin)
		if err != nil {
			return err
		}
		text = string(data)
	}
	e, err := capture.Capture(s, text, capture.Options{
		Source: *source, Agent: *agent, Project: *project, StoreRaw: *raw, NoExcerpt: *noExcerpt,
	})
	if err != nil {
		return err
	}
	fmt.Fprintf(stdout, "captured %s hash=%s source=%s project=%s\n", e.ID, e.Hash[:12], e.Source, e.Project)
	return nil
}

func runHook(s *store.Store, args []string, stdin io.Reader, stdout io.Writer) error {
	fs := flag.NewFlagSet("hook", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	verbose := fs.Bool("verbose", false, "print capture result")
	if err := fs.Parse(reorderFlags(args, map[string]bool{})); err != nil {
		return err
	}
	if len(fs.Args()) == 0 {
		return errors.New("usage: agbox hook <claude|codex>")
	}
	agent := fs.Args()[0]
	data, err := io.ReadAll(stdin)
	if err != nil {
		return err
	}
	text := extractHookText(data)
	e, err := capture.Capture(s, text, capture.Options{
		Source: "hook", Agent: agent, Project: defaultProject(), Redact: true,
	})
	if err != nil {
		return err
	}
	if *verbose {
		fmt.Fprintf(stdout, "hook captured %s hash=%s\n", e.ID, e.Hash[:12])
	}
	return nil
}

func runConnect(args []string, stdout io.Writer) error {
	return runHookConfig(hookconnect.ActionConnect, args, stdout)
}

func runDisconnect(args []string, stdout io.Writer) error {
	return runHookConfig(hookconnect.ActionDisconnect, args, stdout)
}

func runHookConfig(action string, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet(action, flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	apply := fs.Bool("apply", false, "apply the hook config change")
	dryRun := fs.Bool("dry-run", false, "print the hook config plan without writing")
	command := fs.String("command", "", "absolute agbox command path for installed hook")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"command": true})); err != nil {
		return err
	}
	if *apply && *dryRun {
		return errors.New("--apply and --dry-run are mutually exclusive")
	}
	if len(fs.Args()) == 0 {
		return fmt.Errorf("usage: agbox %s <claude|codex|all> [--dry-run|--apply]", action)
	}
	agents, err := hookConfigAgents(fs.Args()[0])
	if err != nil {
		return err
	}
	plans := make([]hookconnect.Plan, 0, len(agents))
	for _, agent := range agents {
		plan, err := hookconnect.BuildPlan(agent, action, hookconnect.Options{Command: *command})
		if err != nil {
			return err
		}
		plans = append(plans, plan)
	}
	var data []byte
	if len(plans) == 1 {
		data, _ = json.MarshalIndent(plans[0], "", "  ")
	} else {
		data, _ = json.MarshalIndent(plans, "", "  ")
	}
	fmt.Fprintln(stdout, string(data))
	if !*apply {
		return nil
	}
	for _, plan := range plans {
		if err := hookconnect.ValidateApply(plan); err != nil {
			return err
		}
	}
	for _, plan := range plans {
		result, err := hookconnect.Apply(plan)
		if err != nil {
			return err
		}
		printHookConfigResult(stdout, action, result)
	}
	return nil
}

func printHookConfigResult(stdout io.Writer, action string, result hookconnect.Result) {
	fmt.Fprintf(stdout, "%s applied agent=%s path=%s changed=%t", action, result.Plan.Agent, result.Plan.Path, result.Changed)
	if result.BackupPath != "" {
		fmt.Fprintf(stdout, " backup=%s", result.BackupPath)
	}
	fmt.Fprintln(stdout)
}

func hookConfigAgents(agent string) ([]string, error) {
	switch strings.ToLower(strings.TrimSpace(agent)) {
	case "all":
		return []string{hookconnect.AgentCodex, hookconnect.AgentClaude}, nil
	case hookconnect.AgentCodex, hookconnect.AgentClaude:
		return []string{strings.ToLower(strings.TrimSpace(agent))}, nil
	default:
		return nil, fmt.Errorf("unsupported agent %q", agent)
	}
}

func runScan(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("scan", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	minRepeats := fs.Int("min-repeats", 2, "minimum repeated signals")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"min-repeats": true})); err != nil {
		return err
	}
	result, err := scan.Run(s, *minRepeats)
	if err != nil {
		return err
	}
	fmt.Fprintf(stdout, "scanned %d events, found %d candidates\n", result.Scanned, len(result.Candidates))
	for _, c := range result.Candidates {
		fmt.Fprintf(stdout, "%s  %s  repeats=%d confidence=%s state=%s\n", c.ID, c.Name, c.EventCount, c.Confidence, c.State)
	}
	return nil
}

func runInbox(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("inbox", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	state := fs.String("state", "", "candidate state filter")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"state": true})); err != nil {
		return err
	}
	candidates, err := s.ListCandidates(*state)
	if err != nil {
		return err
	}
	if len(candidates) == 0 {
		fmt.Fprintln(stdout, "Inbox empty. Run `agbox discover` after a few agent sessions, or test manually with `agbox capture` and `agbox scan`.")
		return nil
	}
	fmt.Fprintln(stdout, "Promotion Inbox")
	for _, c := range candidates {
		fmt.Fprintf(stdout, "%s  %-9s  repeats=%d projects=%d confidence=%s  %s\n", c.ID, c.State, c.EventCount, c.ProjectCount, c.Confidence, c.Name)
	}
	return nil
}

func runDiscover(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("discover", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	minRepeats := fs.Int("min-repeats", 2, "minimum repeated signals")
	state := fs.String("state", string(model.CandidatePending), "candidate state filter, or all")
	limit := fs.Int("limit", 5, "maximum candidates to show")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"min-repeats": true, "state": true, "limit": true})); err != nil {
		return err
	}
	if *limit < 0 {
		return errors.New("--limit must be 0 or greater")
	}
	result, err := scan.Run(s, *minRepeats)
	if err != nil {
		return err
	}
	stateFilter := strings.ToLower(strings.TrimSpace(*state))
	if stateFilter == "all" {
		stateFilter = ""
	}
	candidates, err := s.ListCandidates(stateFilter)
	if err != nil {
		return err
	}
	if *limit > 0 && len(candidates) > *limit {
		candidates = candidates[:*limit]
	}
	if len(candidates) == 0 {
		fmt.Fprintf(stdout, "No workflow candidates yet.\nscanned %d events, found %d repeated signals\n", result.Scanned, len(result.Candidates))
		if result.Scanned == 0 {
			fmt.Fprintln(stdout, "agbox has not captured any prompts in this store.")
		} else {
			fmt.Fprintf(stdout, "Capture at least %d matching prompts before a candidate appears.\n", normalizedMinRepeats(*minRepeats))
		}
		printHookStatus(stdout)
		printDiscoverNext(stdout)
		return nil
	}
	fmt.Fprintf(stdout, "Workflow candidates\nscanned %d events, found %d repeated signals, showing %d %s candidates\n",
		result.Scanned, len(result.Candidates), len(candidates), displayState(stateFilter))
	for i, c := range candidates {
		card, err := evidence.Build(s, c.ID)
		if err != nil {
			return err
		}
		fmt.Fprintf(stdout, "\n%d. %s (%s)\n", i+1, c.Name, c.ID)
		fmt.Fprintf(stdout, "   repeats=%d projects=%d sources=%d confidence=%s state=%s\n",
			c.EventCount, c.ProjectCount, c.SourceCount, c.Confidence, c.State)
		fmt.Fprintf(stdout, "   why: %s\n", card.Reason)
		if len(card.Excerpts) > 0 {
			fmt.Fprintf(stdout, "   excerpt: %s\n", card.Excerpts[0])
		}
		fmt.Fprintf(stdout, "   next: agbox evidence %s\n", c.ID)
		fmt.Fprintf(stdout, "         agbox approve %s --name %s\n", c.ID, c.Name)
		fmt.Fprintf(stdout, "         agbox export %s --target agents-md --dry-run\n", c.ID)
	}
	return nil
}

func runDemo(stdout io.Writer) error {
	dir, err := os.MkdirTemp("", "agbox-demo-*")
	if err != nil {
		return err
	}
	defer os.RemoveAll(dir)
	s, err := store.Open(filepath.Join(dir, "agbox.db"))
	if err != nil {
		return err
	}
	defer s.Close()
	for _, input := range []struct {
		agent string
		text  string
	}{
		{agent: "codex", text: "Use bun, not npm."},
		{agent: "claude", text: "Use bun, not npm."},
	} {
		if _, err := capture.Capture(s, input.text, capture.Options{
			Source: "demo", Agent: input.agent, Project: "agbox-demo", Redact: true,
		}); err != nil {
			return err
		}
	}
	result, err := scan.Run(s, 2)
	if err != nil {
		return err
	}
	if len(result.Candidates) == 0 {
		return errors.New("demo did not produce a candidate")
	}
	c := result.Candidates[0]
	card, err := evidence.Build(s, c.ID)
	if err != nil {
		return err
	}
	if err := s.SetCandidateState(c.ID, model.CandidateApproved, "package-manager-workflow"); err != nil {
		return err
	}
	approved, err := s.GetCandidate(c.ID)
	if err != nil {
		return err
	}
	artifact, err := compile.Render(approved, "agents-md")
	if err != nil {
		return err
	}
	fmt.Fprintln(stdout, "agbox demo: repeated workflow signal detected")
	fmt.Fprintf(stdout, "%s  repeats=%d confidence=%s\n", c.ID, c.EventCount, c.Confidence)
	if len(card.Excerpts) > 0 {
		fmt.Fprintf(stdout, "excerpt: %s\n", card.Excerpts[0])
	}
	fmt.Fprintln(stdout, "\nSkill preview:")
	fmt.Fprintln(stdout, strings.TrimSpace(artifact.Body))
	fmt.Fprintln(stdout, "\nNo files were changed; this demo used a temporary local store.")
	fmt.Fprintln(stdout, "Use this on your own agent sessions:")
	fmt.Fprintln(stdout, "  agbox connect all --apply")
	fmt.Fprintln(stdout, "  agbox discover")
	return nil
}

func normalizedMinRepeats(n int) int {
	if n <= 0 {
		return 2
	}
	return n
}

func displayState(state string) string {
	if strings.TrimSpace(state) == "" {
		return "all"
	}
	return state
}

func printHookStatus(stdout io.Writer) {
	fmt.Fprintln(stdout, "\nHook status")
	for _, status := range hookconnect.StatusAll() {
		line := fmt.Sprintf("- %s: %s", status.Agent, status.State)
		if status.Detail != "" {
			line += " (" + status.Detail + ")"
		}
		fmt.Fprintln(stdout, line)
	}
}

func printDiscoverNext(stdout io.Writer) {
	fmt.Fprintln(stdout, "\nNext")
	fmt.Fprintln(stdout, "1. agbox connect all --apply")
	fmt.Fprintln(stdout, "2. Work normally in Codex or Claude for a few prompts.")
	fmt.Fprintln(stdout, "3. agbox discover")
	fmt.Fprintln(stdout, "\nWant to see the loop without touching your data? Run `agbox demo`.")
}

func runEvidence(s *store.Store, args []string, stdout io.Writer) error {
	if len(args) == 0 {
		return errors.New("usage: agbox evidence <candidate-id>")
	}
	card, err := evidence.Build(s, args[0])
	if err != nil {
		return err
	}
	c := card.Candidate
	fmt.Fprintf(stdout, "%s\nstate: %s\nrepeats: %d\nprojects: %d\nsources: %d\nconfidence: %s\nprivacy: %s\nreason: %s\n",
		c.Name, c.State, c.EventCount, c.ProjectCount, c.SourceCount, c.Confidence, card.Privacy, card.Reason)
	if len(card.Excerpts) > 0 {
		fmt.Fprintln(stdout, "excerpts:")
		for _, ex := range card.Excerpts {
			fmt.Fprintf(stdout, "- %s\n", ex)
		}
	}
	return nil
}

func runState(s *store.Store, args []string, state model.CandidateState, stdout io.Writer) error {
	fs := flag.NewFlagSet(string(state), flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	name := fs.String("name", "", "candidate skill name")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"name": true})); err != nil {
		return err
	}
	if len(fs.Args()) == 0 {
		return fmt.Errorf("usage: agbox %s <candidate-id>", state)
	}
	id := fs.Args()[0]
	if _, err := s.GetCandidate(id); err != nil {
		return err
	}
	if err := s.SetCandidateState(id, state, *name); err != nil {
		return err
	}
	fmt.Fprintf(stdout, "%s -> %s\n", id, state)
	return nil
}

func runCompile(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("compile", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	target := fs.String("target", "agents-md", "target format")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"target": true})); err != nil {
		return err
	}
	if len(fs.Args()) == 0 {
		return errors.New("usage: agbox compile <candidate-id> [--target agents-md|claude|codex|cursor|cline]")
	}
	c, err := s.GetCandidate(fs.Args()[0])
	if err != nil {
		return err
	}
	artifact, err := compile.Render(c, *target)
	if err != nil {
		return err
	}
	fmt.Fprintln(stdout, artifact.Body)
	return nil
}

func runExport(s *store.Store, args []string, stdout io.Writer) error {
	if len(args) > 0 && args[0] == "rollback" {
		if len(args) < 2 {
			return errors.New("usage: agbox export rollback <export-id>")
		}
		root, err := fsx.ProjectRoot()
		if err != nil {
			return err
		}
		rec, err := agexport.Rollback(s, root, args[1])
		if err != nil {
			return err
		}
		fmt.Fprintf(stdout, "rolled back %s path=%s\n", rec.ID, rec.Path)
		return nil
	}
	fs := flag.NewFlagSet("export", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	target := fs.String("target", "agents-md", "export target")
	path := fs.String("path", "", "relative export path")
	dryRun := fs.Bool("dry-run", false, "print export plan without writing")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"target": true, "path": true})); err != nil {
		return err
	}
	root, err := fsx.ProjectRoot()
	if err != nil {
		return err
	}
	candidates, err := exportCandidates(s, fs.Args())
	if err != nil {
		return err
	}
	if len(candidates) == 0 {
		return errors.New("no approved candidates to export")
	}
	for _, c := range candidates {
		opts := agexport.Options{Target: *target, Path: *path, DryRun: *dryRun}
		if *dryRun {
			plan, _, err := agexport.BuildPlan(root, c, opts)
			if err != nil {
				return err
			}
			data, _ := json.MarshalIndent(plan, "", "  ")
			fmt.Fprintln(stdout, string(data))
			continue
		}
		rec, err := agexport.Apply(s, root, c, opts)
		if err != nil {
			return err
		}
		fmt.Fprintf(stdout, "exported %s candidate=%s target=%s path=%s\n", rec.ID, c.ID, rec.Target, rec.Path)
	}
	return nil
}

func runManifest(args []string, stdout io.Writer) error {
	if len(args) == 0 || args[0] != "verify" {
		return errors.New("usage: agbox manifest verify")
	}
	root, err := fsx.ProjectRoot()
	if err != nil {
		return err
	}
	if err := manifest.Verify(root); err != nil {
		return err
	}
	fmt.Fprintln(stdout, "manifest OK")
	return nil
}

func runImpact(s *store.Store, args []string, stdout io.Writer) error {
	if len(args) == 0 {
		return errors.New("usage: agbox impact <candidate-id>")
	}
	meter, err := impact.Build(s, args[0])
	if err != nil {
		return err
	}
	fmt.Fprintf(stdout, "candidate=%s before=%d after=%d reduction=%d confidence=%s\n%s\n",
		meter.CandidateID, meter.Before, meter.After, meter.Reduction, meter.Confidence, meter.Window)
	return nil
}

func runAudit(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("audit", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	profile := fs.String("profile", "private", "private|shareable|client")
	out := fs.String("out", "", "output markdown path")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"profile": true, "out": true})); err != nil {
		return err
	}
	md, err := audit.Render(s, *profile)
	if err != nil {
		return err
	}
	if *out == "" {
		fmt.Fprint(stdout, md)
		return nil
	}
	if err := os.WriteFile(*out, []byte(md), 0o644); err != nil {
		return err
	}
	fmt.Fprintf(stdout, "audit written: %s\n", *out)
	return nil
}

func runDoctor(s *store.Store, stdout io.Writer) error {
	report := doctor.Run(s)
	for _, line := range report.Lines {
		fmt.Fprintln(stdout, line)
	}
	if !report.OK {
		return errors.New("doctor found problems")
	}
	return nil
}

func runDebugBundle(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("debug-bundle", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	out := fs.String("out", "agbox-debug-bundle.txt", "output path")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"out": true})); err != nil {
		return err
	}
	path, err := doctor.DebugBundle(s, *out)
	if err != nil {
		return err
	}
	fmt.Fprintf(stdout, "debug bundle written: %s\n", path)
	return nil
}

func runRepair(s *store.Store, stdout io.Writer) error {
	root, err := fsx.ProjectRoot()
	if err != nil {
		return err
	}
	if err := agexport.Repair(s, root); err != nil {
		return err
	}
	fmt.Fprintln(stdout, "repair complete")
	return nil
}

func exportCandidates(s *store.Store, ids []string) ([]model.Candidate, error) {
	if len(ids) > 0 {
		out := make([]model.Candidate, 0, len(ids))
		for _, id := range ids {
			c, err := s.GetCandidate(id)
			if err != nil {
				return nil, err
			}
			out = append(out, c)
		}
		return out, nil
	}
	candidates, err := s.ListCandidates(string(model.CandidateApproved))
	if err != nil {
		return nil, err
	}
	return candidates, nil
}

func extractHookText(data []byte) string {
	var payload map[string]any
	if err := json.Unmarshal(data, &payload); err == nil {
		for _, key := range []string{"prompt", "message", "text", "content", "input"} {
			if v, ok := payload[key].(string); ok && strings.TrimSpace(v) != "" {
				return v
			}
		}
	}
	return string(data)
}

func reorderFlags(args []string, valueFlags map[string]bool) []string {
	flags := make([]string, 0, len(args))
	positionals := make([]string, 0, len(args))
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if strings.HasPrefix(arg, "-") && arg != "-" {
			flags = append(flags, arg)
			name := strings.TrimLeft(arg, "-")
			if idx := strings.IndexByte(name, '='); idx >= 0 {
				name = name[:idx]
			}
			if valueFlags[name] && !strings.Contains(arg, "=") && i+1 < len(args) {
				flags = append(flags, args[i+1])
				i++
			}
			continue
		}
		positionals = append(positionals, arg)
	}
	return append(flags, positionals...)
}

func ensureGitignore(entry string) error {
	existing, err := os.ReadFile(".gitignore")
	if err != nil && !os.IsNotExist(err) {
		return err
	}
	if strings.Contains(string(existing), entry) {
		return nil
	}
	data := strings.TrimRight(string(existing), "\n")
	if data != "" {
		data += "\n"
	}
	data += entry + "\n"
	return os.WriteFile(".gitignore", []byte(data), 0o644)
}

func defaultProject() string {
	wd, err := os.Getwd()
	if err != nil {
		return "default"
	}
	return filepath.Base(wd)
}

func printUsage(w io.Writer) {
	fmt.Fprintln(w, `agbox turns repeated AI-agent workflow signals into reusable skills.

Usage:
  agbox init
  agbox capture [--source manual] [--agent codex] "Use bun, not npm"
  agbox scan
  agbox inbox [--state pending]
  agbox evidence <candidate-id>
  agbox approve <candidate-id> [--name api-change-workflow]
  agbox compile <candidate-id> [--target agents-md|claude|codex|cursor|cline]
  agbox export [candidate-id...] [--target agents-md] [--dry-run]
  agbox export rollback <export-id>
  agbox hook <claude|codex> [--verbose]
  agbox connect <claude|codex|all> [--dry-run|--apply] [--command /path/to/agbox]
  agbox disconnect <claude|codex|all> [--dry-run|--apply]
  agbox discover [--min-repeats 2] [--state pending|all] [--limit 5]
  agbox demo
  agbox impact <candidate-id>
  agbox audit [--profile private|shareable|client] [--out audit.md]
  agbox manifest verify
  agbox doctor
  agbox repair`)
}
