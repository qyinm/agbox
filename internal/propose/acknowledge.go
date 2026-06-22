package propose

import (
	"encoding/json"
	"path/filepath"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/store"
)

type postToolInput struct {
	ToolInput struct {
		FilePath string `json:"file_path"`
		Path     string `json:"path"`
	} `json:"tool_input"`
	ToolName string `json:"tool_name"`
}

func Acknowledge(s *store.Store, agent string, hookData []byte) error {
	path := extractSkillPath(hookData)
	if path == "" || !MatchesSkillPath(agent, path) {
		return nil
	}
	project := filepath.Base(filepath.Dir(filepath.Dir(path)))
	c, err := s.LatestProposedCandidate(project)
	if err != nil {
		candidates, listErr := s.ListCandidatesByState(model.CandidateProposed)
		if listErr != nil || len(candidates) == 0 {
			return nil
		}
		c = candidates[0]
	}
	now := time.Now()
	return s.UpdateCandidateMeta(c.ID, store.CandidateMetaUpdate{
		State:     model.CandidateAccepted,
		SkillPath: path,
		ProposedAt: func() *time.Time {
			t := now
			return &t
		}(),
	})
}

func extractSkillPath(data []byte) string {
	var in postToolInput
	if err := json.Unmarshal(data, &in); err != nil {
		return ""
	}
	if p := strings.TrimSpace(in.ToolInput.FilePath); p != "" {
		return p
	}
	return strings.TrimSpace(in.ToolInput.Path)
}

func Accept(s *store.Store, candidateID, skillPath string) error {
	now := time.Now()
	return s.UpdateCandidateMeta(candidateID, store.CandidateMetaUpdate{
		State:     model.CandidateAccepted,
		SkillPath: skillPath,
		ProposedAt: func() *time.Time {
			t := now
			return &t
		}(),
	})
}

func Snooze(s *store.Store, candidateID string) error {
	until := time.Now().Add(snoozeCooldown)
	return s.UpdateCandidateMeta(candidateID, store.CandidateMetaUpdate{
		State:        model.CandidateSnoozed,
		SnoozedUntil: &until,
	})
}

func Reject(s *store.Store, candidateID string) error {
	return s.UpdateCandidateMeta(candidateID, store.CandidateMetaUpdate{
		State: model.CandidateRejected,
	})
}