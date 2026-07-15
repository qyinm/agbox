package cli

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"sort"
	"time"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
)

func runSources(s *store.Store, args []string, stdout io.Writer) error {
	if len(args) > 0 && args[0] == "resume" {
		return runSourcesResume(s, args[1:], stdout)
	}
	if len(args) > 0 {
		return errors.New("usage: agbox sources resume <opaque-source-id> --generation N")
	}
	type entry struct {
		agent string
		path  string
	}
	var entries []entry
	for _, adapter := range session.All() {
		sources, err := adapter.DiscoverSources()
		if err != nil {
			return err
		}
		for _, src := range sources {
			entries = append(entries, entry{agent: src.Agent, path: src.Path})
		}
	}
	sort.Slice(entries, func(i, j int) bool {
		if entries[i].agent == entries[j].agent {
			return entries[i].path < entries[j].path
		}
		return entries[i].agent < entries[j].agent
	})
	if len(entries) == 0 {
		fmt.Fprintln(stdout, "No session sources discovered.")
		return nil
	}
	for _, e := range entries {
		fmt.Fprintf(stdout, "%-8s %s\n", e.agent, e.path)
	}
	return nil
}

func runSourcesResume(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("sources resume", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	generation := fs.Int64("generation", 0, "expected source generation")
	if err := fs.Parse(reorderFlags(args, map[string]bool{"generation": true})); err != nil {
		return err
	}
	if fs.NArg() != 1 || *generation <= 0 {
		return errors.New("usage: agbox sources resume <opaque-source-id> --generation N")
	}
	opaqueID := fs.Arg(0)
	if err := s.ResumeSourceByOpaqueID(opaqueID, *generation, time.Now()); err != nil {
		switch {
		case errors.Is(err, store.ErrGenerationMismatch):
			return errors.New("source resume rejected: source was not found or its generation was replaced")
		case errors.Is(err, store.ErrStateConflict):
			return errors.New("source resume rejected: source is not quarantined")
		default:
			return errors.New("source resume failed")
		}
	}
	fmt.Fprintf(stdout, "source resumed: %s generation=%d\n", opaqueID, *generation)
	return nil
}
