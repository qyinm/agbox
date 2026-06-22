package cli

import (
	"fmt"
	"io"
	"sort"

	"github.com/hippoom/agbox/internal/session"
)

func runSources(stdout io.Writer) error {
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