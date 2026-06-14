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
	byFingerprint := map[string][]model.Event{}
	for _, e := range events {
		fingerprint := clusterFingerprint(e)
		byFingerprint[fingerprint] = append(byFingerprint[fingerprint], e)
	}
	var result Result
	result.Scanned = len(events)
	for fingerprint, group := range byFingerprint {
		if len(group) < minRepeats {
			continue
		}
		c := buildCandidate(fingerprint, group)
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

func buildCandidate(fingerprint string, events []model.Event) model.Candidate {
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
		ruleText = "Workflow signal " + fingerprint[:12]
	}
	name := privacy.Slug(ruleText)
	if kind := workflowKind(events); kind != "" {
		name = privacy.Slug(kind)
	}
	confidence := "medium"
	if len(events) >= 5 || len(projects) >= 2 {
		confidence = "high"
	}
	if len(events) == 2 && len(projects) == 1 {
		confidence = "low"
	}
	now := time.Now()
	return model.Candidate{
		ID:           "cand_" + fingerprint[:12],
		Fingerprint:  fingerprint,
		Name:         name,
		Description:  workflowDescription(events),
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
