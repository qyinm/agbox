package propose

import (
	"encoding/json"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
	proposestate "github.com/hippoom/agbox/internal/propose/state"
	"github.com/hippoom/agbox/internal/store"
)

type postToolInput struct {
	ToolInput struct {
		FilePath string `json:"file_path"`
		Path     string `json:"path"`
	} `json:"tool_input"`
	ToolName string `json:"tool_name"`
	CWD      string `json:"cwd"`
}

var (
	candidateIDFrontmatter = regexp.MustCompile(`(?m)^agbox_candidate_id:\s*(\S+)`)
	candidateIDComment     = regexp.MustCompile(`<!--\s*agbox:candidate\s+(\S+)\s*-->`)
)

func Acknowledge(s *store.Store, agent string, hookData []byte) error {
	path := resolveSkillPath(hookData)
	if path == "" || !MatchesSkillPath(agent, path) {
		return nil
	}
	content, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	candidateID := extractCandidateID(string(content))
	if candidateID == "" {
		return nil
	}
	c, err := s.GetCandidate(candidateID)
	if err != nil || !canAcknowledgeToAccepted(c.State) {
		return nil
	}
	if project := ProjectFromHook(hookData); project != "" && !candidateMatchesProject(s, c.ID, project) {
		return nil
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

func extractCandidateID(content string) string {
	if m := candidateIDFrontmatter.FindStringSubmatch(content); len(m) == 2 {
		return strings.TrimSpace(m[1])
	}
	if m := candidateIDComment.FindStringSubmatch(content); len(m) == 2 {
		return strings.TrimSpace(m[1])
	}
	return ""
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

func resolveSkillPath(data []byte) string {
	var in postToolInput
	if err := json.Unmarshal(data, &in); err != nil {
		return ""
	}
	path := strings.TrimSpace(in.ToolInput.FilePath)
	if path == "" {
		path = strings.TrimSpace(in.ToolInput.Path)
	}
	if path == "" {
		return ""
	}
	if filepath.IsAbs(path) {
		return filepath.Clean(path)
	}
	if cwd := strings.TrimSpace(in.CWD); cwd != "" {
		return filepath.Clean(filepath.Join(cwd, path))
	}
	return filepath.Clean(path)
}

func Accept(s *store.Store, candidateID, skillPath string) error {
	if _, err := s.GetCandidate(candidateID); err != nil {
		return err
	}
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

func ApplyOnce(s *store.Store, app model.ReplayApplication) error {
	if _, err := s.GetCandidate(app.CandidateID); err != nil {
		return err
	}
	_, err := s.RecordReplayApplication(app)
	return err
}

func canAcknowledgeToAccepted(state model.CandidateState) bool {
	switch state {
	case model.CandidateProposed, model.CandidateSaveSuggested:
		return true
	default:
		return false
	}
}

func Snooze(s *store.Store, candidateID string) error {
	if _, err := s.GetCandidate(candidateID); err != nil {
		return err
	}
	until := time.Now().Add(proposestate.SnoozeCooldown)
	return s.UpdateCandidateMeta(candidateID, store.CandidateMetaUpdate{
		State:        model.CandidateSnoozed,
		SnoozedUntil: &until,
	})
}

func Reject(s *store.Store, candidateID string) error {
	if _, err := s.GetCandidate(candidateID); err != nil {
		return err
	}
	return s.UpdateCandidateMeta(candidateID, store.CandidateMetaUpdate{
		State: model.CandidateRejected,
	})
}
