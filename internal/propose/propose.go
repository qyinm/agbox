package propose

import (
	"encoding/json"
	"path/filepath"
	"sort"
	"time"

	"github.com/hippoom/agbox/internal/evidence"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/store"
)

type HookInput struct {
	CWD string `json:"cwd"`
}

func Propose(s *store.Store, agent, project string) (string, error) {
	candidates, err := s.ListCandidatesByState(model.CandidateProposalReady)
	if err != nil {
		return "", err
	}
	now := time.Now()
	var eligible []model.Candidate
	for _, c := range candidates {
		if InCooldown(c, now) {
			continue
		}
		if project != "" && !candidateMatchesProject(s, c.ID, project) {
			continue
		}
		eligible = append(eligible, c)
	}
	if len(eligible) == 0 {
		return "", nil
	}
	sort.Slice(eligible, func(i, j int) bool {
		rank := func(c model.Candidate) int {
			switch c.Confidence {
			case "high":
				return 3
			case "medium":
				return 2
			default:
				return 1
			}
		}
		ri, rj := rank(eligible[i]), rank(eligible[j])
		if ri != rj {
			return ri > rj
		}
		if eligible[i].EventCount != eligible[j].EventCount {
			return eligible[i].EventCount > eligible[j].EventCount
		}
		return eligible[i].LastSeen.After(eligible[j].LastSeen)
	})
	top := eligible[0]
	card, err := evidence.Build(s, top.ID)
	if err != nil {
		return "", err
	}
	proposedAt := now
	if err := s.UpdateCandidateMeta(top.ID, store.CandidateMetaUpdate{
		State:      model.CandidateProposed,
		ProposedAt: &proposedAt,
	}); err != nil {
		return "", err
	}
	return RenderInjection(agent, card), nil
}

func ParseHookInput(data []byte) HookInput {
	var in HookInput
	_ = json.Unmarshal(data, &in)
	return in
}

func ProjectFromHook(data []byte) string {
	in := ParseHookInput(data)
	if in.CWD != "" {
		return filepath.Base(in.CWD)
	}
	return ""
}

func candidateMatchesProject(s *store.Store, candidateID, project string) bool {
	corrections, err := s.CorrectionsForCandidate(candidateID)
	if err == nil {
		for _, cor := range corrections {
			if cor.Project == project {
				return true
			}
		}
	}
	events, err := s.EventsForCandidate(candidateID)
	if err == nil {
		for _, e := range events {
			if e.Project == project {
				return true
			}
		}
	}
	return false
}