package evidence

import (
	"fmt"
	"strings"

	"sort"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/store"
)

func Build(s *store.Store, candidateID string) (model.EvidenceCard, error) {
	c, err := s.GetCandidate(candidateID)
	if err != nil {
		return model.EvidenceCard{}, err
	}

	corrections, err := s.CorrectionsForCandidate(candidateID)
	if err != nil {
		return model.EvidenceCard{}, err
	}
	if c.SourceKind == model.CandidateSourceCorrection && len(corrections) > 0 {
		return buildFromCorrections(s, c, corrections)
	}
	if c.SourceKind == "" && len(corrections) > 0 {
		return buildFromCorrections(s, c, corrections)
	}
	return buildFromEvents(s, c, candidateID)
}

func buildFromCorrections(s *store.Store, c model.Candidate, corrections []model.Correction) (model.EvidenceCard, error) {
	sources := set()
	projects := set()
	agents := set()
	excerpts := make([]string, 0, 3)
	occurrences := make([]model.Occurrence, 0, len(corrections))

	for _, cor := range corrections {
		sources[cor.Agent] = true
		projects[cor.Project] = true
		agents[cor.Agent] = true
		if cor.Excerpt != "" && len(excerpts) < 3 {
			excerpts = append(excerpts, cor.Excerpt)
		}
		occ, err := buildOccurrence(s, cor)
		if err != nil {
			return model.EvidenceCard{}, err
		}
		occurrences = append(occurrences, occ)
	}

	privacyMode := "hash+metadata+redacted-excerpt"
	return model.EvidenceCard{
		Candidate:   c,
		Sources:     sortedKeys(sources),
		Projects:    sortedKeys(projects),
		Agents:      sortedKeys(agents),
		Excerpts:    excerpts,
		Occurrences: occurrences,
		Reason:      reason(c),
		Privacy:     privacyMode,
	}, nil
}

func buildOccurrence(s *store.Store, cor model.Correction) (model.Occurrence, error) {
	action, err := s.GetAction(cor.ActionID)
	if err != nil {
		action = model.Action{Excerpt: cor.Excerpt}
	}

	agentTurn, err := s.GetTurn(action.TurnID)
	if err != nil {
		agentTurn = model.Turn{Role: "agent", CreatedAt: cor.CreatedAt}
	}
	userTurn, err := s.GetTurn(cor.TurnID)
	if err != nil {
		userTurn = model.Turn{Role: "user", CreatedAt: cor.CreatedAt}
	}

	drillDown := []model.DrillStep{
		{
			TurnIndex: agentTurn.TurnIndex,
			Role:      agentTurn.Role,
			Summary:   actionDrillSummary(action),
			CreatedAt: agentTurn.CreatedAt,
		},
		{
			TurnIndex: userTurn.TurnIndex,
			Role:      userTurn.Role,
			Summary:   cor.Excerpt,
			CreatedAt: userTurn.CreatedAt,
		},
	}

	return model.Occurrence{
		ID:             cor.ID,
		SessionID:      cor.SessionID,
		CreatedAt:      cor.CreatedAt,
		AgentAction:    actionOneLine(action),
		UserCorrection: cor.Excerpt,
		DrillDown:      drillDown,
	}, nil
}

func actionOneLine(action model.Action) string {
	if cmd := strings.TrimSpace(action.Command); cmd != "" {
		return fmt.Sprintf("ran `%s`", cmd)
	}
	if action.ToolName != "" && action.FilePath != "" {
		return fmt.Sprintf("edited `%s`", action.FilePath)
	}
	if action.ToolName != "" {
		return fmt.Sprintf("used `%s`", action.ToolName)
	}
	if excerpt := strings.TrimSpace(action.Excerpt); excerpt != "" {
		return fmt.Sprintf("ran `%s`", excerpt)
	}
	return "agent action"
}

func actionDrillSummary(action model.Action) string {
	if cmd := strings.TrimSpace(action.Command); cmd != "" {
		return "Ran: " + cmd
	}
	if action.ToolName != "" && action.FilePath != "" {
		return fmt.Sprintf("tool:%s  file:%q", action.ToolName, action.FilePath)
	}
	if action.ToolName != "" {
		return "tool:" + action.ToolName
	}
	if excerpt := strings.TrimSpace(action.Excerpt); excerpt != "" {
		return "Ran: " + excerpt
	}
	return "agent action"
}

func buildFromEvents(s *store.Store, c model.Candidate, candidateID string) (model.EvidenceCard, error) {
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
	if c.SourceKind == model.CandidateSourcePromptPattern {
		if c.ProjectCount > 1 {
			return "Repeated prompt pattern across multiple projects; likely a durable workflow request."
		}
		if c.EventCount >= 5 {
			return "Repeated prompt pattern in this project; likely worth turning into reusable guidance."
		}
		return "Repeated prompt pattern; review before promoting."
	}
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
