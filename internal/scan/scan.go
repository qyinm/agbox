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
	count, err := s.CountCorrections()
	if err != nil {
		return Result{}, err
	}
	if count > 0 {
		return runCorrections(s, minRepeats)
	}
	return runEvents(s, minRepeats)
}

func runEvents(s *store.Store, minRepeats int) (Result, error) {
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
		if err := s.UpsertCandidate(c, ids, nil); err != nil {
			return Result{}, err
		}
		result.Candidates = append(result.Candidates, c)
	}
	sortCandidates(&result)
	return result, nil
}

func runCorrections(s *store.Store, minRepeats int) (Result, error) {
	corrections, err := s.ListCorrections()
	if err != nil {
		return Result{}, err
	}
	actionCache := map[string]model.Action{}
	byFingerprint := map[string][]model.Correction{}
	for _, c := range corrections {
		action, ok := actionCache[c.ActionID]
		if !ok {
			action, err = s.GetAction(c.ActionID)
			if err != nil {
				action = model.Action{Excerpt: c.Excerpt}
			}
			actionCache[c.ActionID] = action
		}
		fingerprint := correctionClusterFingerprint(c, action)
		byFingerprint[fingerprint] = append(byFingerprint[fingerprint], c)
	}
	var result Result
	result.Scanned = len(corrections)
	for fingerprint, group := range byFingerprint {
		if len(group) < minRepeats {
			continue
		}
		c := buildCandidateFromCorrections(fingerprint, group)
		ids := make([]string, 0, len(group))
		for _, cor := range group {
			ids = append(ids, cor.ID)
		}
		if err := s.UpsertCandidate(c, nil, ids); err != nil {
			return Result{}, err
		}
		result.Candidates = append(result.Candidates, c)
	}
	sortCandidates(&result)
	return result, nil
}

func correctionClusterFingerprint(c model.Correction, action model.Action) string {
	return privacy.HashSignal(c.Normalized + "|" + actionFingerprint(action))
}

func actionFingerprint(action model.Action) string {
	if norm := privacy.NormalizeSignal(action.Command); norm != "" {
		return norm
	}
	return privacy.NormalizeSignal(action.Excerpt)
}

func buildCandidateFromCorrections(fingerprint string, corrections []model.Correction) model.Candidate {
	events := make([]model.Event, len(corrections))
	for i, c := range corrections {
		events[i] = model.Event{
			Normalized: c.Normalized,
			Excerpt:    c.Excerpt,
			Project:    c.Project,
			Source:     c.Agent,
			CreatedAt:  c.CreatedAt,
		}
	}
	return buildCandidate(fingerprint, events)
}

func sortCandidates(result *Result) {
	sort.Slice(result.Candidates, func(i, j int) bool {
		if result.Candidates[i].EventCount == result.Candidates[j].EventCount {
			return result.Candidates[i].LastSeen.After(result.Candidates[j].LastSeen)
		}
		return result.Candidates[i].EventCount > result.Candidates[j].EventCount
	})
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