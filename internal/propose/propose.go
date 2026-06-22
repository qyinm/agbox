package propose

import (
	"encoding/json"
	"io"
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

// SelectAndRender picks the top proposal_ready candidate and renders injection text
// without mutating candidate state. Call MarkProposed after stdout write succeeds.
func SelectAndRender(s *store.Store, agent, project string) (candidateID, payload string, err error) {
	candidates, err := s.ListCandidatesByState(model.CandidateProposalReady)
	if err != nil {
		return "", "", err
	}
	var eligible []model.Candidate
	for _, c := range candidates {
		if project != "" && !candidateMatchesProject(s, c.ID, project) {
			continue
		}
		eligible = append(eligible, c)
	}
	if len(eligible) == 0 {
		return "", "", nil
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
		return "", "", err
	}
	return top.ID, RenderInjection(agent, card), nil
}

// DeliverProposed writes injection text to the hook stdout, then marks the candidate proposed.
// If marking fails after stdout delivery, it logs a warning instead of returning an error so
// hook retries do not duplicate the injection payload.
func DeliverProposed(s *store.Store, candidateID, payload string, stdout, log io.Writer) error {
	if _, err := io.WriteString(stdout, payload); err != nil {
		return err
	}
	if err := MarkProposed(s, candidateID); err != nil && log != nil {
		_, _ = io.WriteString(log, "agbox: warning: proposal "+candidateID+" delivered but state not updated: "+err.Error()+"\n")
	}
	return nil
}

// MarkProposed transitions a candidate to proposed after successful stdout delivery.
func MarkProposed(s *store.Store, candidateID string) error {
	c, err := s.GetCandidate(candidateID)
	if err != nil {
		return err
	}
	if c.State != model.CandidateProposalReady {
		return nil
	}
	now := time.Now()
	return s.UpdateCandidateMeta(candidateID, store.CandidateMetaUpdate{
		State:      model.CandidateProposed,
		ProposedAt: &now,
	})
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
	if err == nil && len(corrections) > 0 {
		for _, cor := range corrections {
			if cor.Project == project {
				return true
			}
		}
		return false
	}
	events, err := s.EventsForCandidate(candidateID)
	if err == nil && len(events) > 0 {
		for _, e := range events {
			if e.Project == project {
				return true
			}
		}
		return false
	}
	return true
}