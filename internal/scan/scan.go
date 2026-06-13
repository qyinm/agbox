package scan

import (
	"sort"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
	"github.com/hippoom/agbox/internal/store"
)

type Result struct {
	Candidates []model.Candidate
	Scanned    int
}

func Run(s *store.Store, minRepeats int) (Result, error) {
	if minRepeats <= 0 {
		minRepeats = 2
	}
	events, err := s.ListEvents()
	if err != nil {
		return Result{}, err
	}
	byHash := map[string][]model.Event{}
	for _, e := range events {
		byHash[e.Hash] = append(byHash[e.Hash], e)
	}
	var result Result
	result.Scanned = len(events)
	for hash, group := range byHash {
		if len(group) < minRepeats {
			continue
		}
		c := buildCandidate(hash, group)
		ids := make([]string, 0, len(group))
		for _, e := range group {
			ids = append(ids, e.ID)
		}
		if err := s.UpsertCandidate(c, ids); err != nil {
			return Result{}, err
		}
		result.Candidates = append(result.Candidates, c)
	}
	sort.Slice(result.Candidates, func(i, j int) bool {
		if result.Candidates[i].EventCount == result.Candidates[j].EventCount {
			return result.Candidates[i].LastSeen.After(result.Candidates[j].LastSeen)
		}
		return result.Candidates[i].EventCount > result.Candidates[j].EventCount
	})
	return result, nil
}

func buildCandidate(hash string, events []model.Event) model.Candidate {
	first := events[0].CreatedAt
	last := events[0].CreatedAt
	projects := map[string]bool{}
	sources := map[string]bool{}
	excerpt := ""
	for _, e := range events {
		if e.CreatedAt.Before(first) {
			first = e.CreatedAt
		}
		if e.CreatedAt.After(last) {
			last = e.CreatedAt
		}
		projects[e.Project] = true
		sources[e.Source] = true
		if excerpt == "" && strings.TrimSpace(e.Excerpt) != "" {
			excerpt = e.Excerpt
		}
	}
	ruleText := excerpt
	if ruleText == "" {
		ruleText = "Workflow signal " + hash[:12]
	}
	name := privacy.Slug(ruleText)
	confidence := "medium"
	if len(events) >= 5 || len(projects) >= 2 {
		confidence = "high"
	}
	if len(events) == 2 && len(projects) == 1 {
		confidence = "low"
	}
	now := time.Now()
	return model.Candidate{
		ID:           "cand_" + hash[:12],
		Fingerprint:  hash,
		Name:         name,
		Description:  "Use when this repeated workflow instruction appears in agent sessions.",
		RuleText:     ruleText,
		State:        model.CandidatePending,
		EventCount:   len(events),
		ProjectCount: len(projects),
		SourceCount:  len(sources),
		FirstSeen:    first,
		LastSeen:     last,
		Confidence:   confidence,
		Version:      1,
		UpdatedAt:    now,
	}
}
