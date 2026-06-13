package evidence

import (
	"sort"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/store"
)

func Build(s *store.Store, candidateID string) (model.EvidenceCard, error) {
	c, err := s.GetCandidate(candidateID)
	if err != nil {
		return model.EvidenceCard{}, err
	}
	events, err := s.EventsForCandidate(candidateID)
	if err != nil {
		return model.EvidenceCard{}, err
	}
	sources := set()
	projects := set()
	agents := set()
	excerpts := make([]string, 0, 3)
	rawSeen := false
	for _, e := range events {
		sources[e.Source] = true
		projects[e.Project] = true
		agents[e.Agent] = true
		if e.RawStored {
			rawSeen = true
		}
		if e.Excerpt != "" && len(excerpts) < 3 {
			excerpts = append(excerpts, e.Excerpt)
		}
	}
	privacyMode := "hash+metadata"
	if len(excerpts) > 0 {
		privacyMode = "hash+metadata+redacted-excerpt"
	}
	if rawSeen {
		privacyMode = "raw-opt-in"
	}
	return model.EvidenceCard{
		Candidate: c,
		Sources:   sortedKeys(sources),
		Projects:  sortedKeys(projects),
		Agents:    sortedKeys(agents),
		Excerpts:  excerpts,
		Reason:    reason(c),
		Privacy:   privacyMode,
	}, nil
}

func reason(c model.Candidate) string {
	if c.ProjectCount > 1 {
		return "Repeated across multiple projects; likely a durable workflow preference."
	}
	if c.EventCount >= 5 {
		return "Repeated frequently in this project; likely worth promoting."
	}
	return "Repeated at least twice; review before promoting."
}

func set() map[string]bool {
	return map[string]bool{}
}

func sortedKeys(m map[string]bool) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}
