package propose

import (
	"encoding/json"
	"io"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/evidence"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/store"
)

type HookInput struct {
	CWD         string `json:"cwd"`
	Prompt      string `json:"prompt"`
	UserPrompt  string `json:"user_prompt"`
	UserPromptC string `json:"userPrompt"`
	Message     string `json:"message"`
	Text        string `json:"text"`
	Input       string `json:"input"`
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
		ri, rj := confidenceRank(eligible[i]), confidenceRank(eligible[j])
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

// SelectAndRenderForPrompt uses the prompt-submit hook payload as the primary
// matching signal. If the current prompt is missing or unrelated, it emits no
// payload instead of falling back to a generic project-level proposal.
func SelectAndRenderForPrompt(s *store.Store, agent, project, prompt string) (candidateID, payload string, err error) {
	match, err := SelectForPrompt(s, project, prompt)
	if err != nil || match.ID == "" {
		return "", "", err
	}
	card, err := evidence.Build(s, match.ID)
	if err != nil {
		return "", "", err
	}
	return match.ID, RenderInjection(agent, card), nil
}

func SelectForPrompt(s *store.Store, project, prompt string) (model.Candidate, error) {
	normalized := privacy.NormalizeSignal(prompt)
	if normalized == "" {
		return model.Candidate{}, nil
	}
	semanticKey := scan.SemanticKey(normalized)
	exactFingerprint := privacy.HashSignal(string(model.CandidateSourcePromptPattern) + ":exact:" + privacy.HashSignal(normalized))
	semanticFingerprint := ""
	if semanticKey != "" {
		semanticFingerprint = privacy.HashSignal(string(model.CandidateSourcePromptPattern) + ":semantic:" + semanticKey)
	}

	candidates, err := s.ListCandidatesByState(model.CandidateProposalReady)
	if err != nil {
		return model.Candidate{}, err
	}
	var eligible []model.Candidate
	for _, c := range candidates {
		if !eligiblePromptReplayCandidate(c) {
			continue
		}
		if project != "" && !candidateMatchesProject(s, c.ID, project) {
			continue
		}
		if !candidateMatchesPrompt(c, semanticKey, semanticFingerprint, exactFingerprint) {
			continue
		}
		eligible = append(eligible, c)
	}
	if len(eligible) == 0 {
		return model.Candidate{}, nil
	}
	sortCandidatesForProposal(eligible)
	return eligible[0], nil
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

func PromptFromHook(data []byte) string {
	in := ParseHookInput(data)
	for _, value := range []string{in.Prompt, in.UserPrompt, in.UserPromptC, in.Message, in.Text, in.Input} {
		if prompt := cleanPrompt(value); prompt != "" {
			return prompt
		}
	}
	var raw map[string]any
	if err := json.Unmarshal(data, &raw); err != nil {
		return ""
	}
	for _, key := range []string{"prompt", "user_prompt", "userPrompt", "message", "text", "input", "content"} {
		if prompt := promptString(raw[key]); prompt != "" {
			return prompt
		}
	}
	for _, key := range []string{"payload", "event", "data"} {
		child, ok := raw[key].(map[string]any)
		if !ok {
			continue
		}
		for _, promptKey := range []string{"prompt", "user_prompt", "userPrompt", "message", "text", "input", "content"} {
			if prompt := promptString(child[promptKey]); prompt != "" {
				return prompt
			}
		}
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

func sortCandidatesForProposal(candidates []model.Candidate) {
	sort.Slice(candidates, func(i, j int) bool {
		ri, rj := confidenceRank(candidates[i]), confidenceRank(candidates[j])
		if ri != rj {
			return ri > rj
		}
		if candidates[i].EventCount != candidates[j].EventCount {
			return candidates[i].EventCount > candidates[j].EventCount
		}
		return candidates[i].LastSeen.After(candidates[j].LastSeen)
	})
}

func confidenceRank(c model.Candidate) int {
	switch c.Confidence {
	case "high":
		return 3
	case "medium":
		return 2
	default:
		return 1
	}
}

func eligiblePromptReplayCandidate(c model.Candidate) bool {
	if c.SourceKind != model.CandidateSourcePromptPattern {
		return false
	}
	if c.Confidence == "low" && c.ProjectCount <= 1 {
		return false
	}
	return true
}

func candidateMatchesPrompt(c model.Candidate, semanticKey, semanticFingerprint, exactFingerprint string) bool {
	if semanticKey != "" && c.SemanticKey == semanticKey {
		return true
	}
	if semanticFingerprint != "" && c.Fingerprint == semanticFingerprint {
		return true
	}
	return c.Fingerprint == exactFingerprint
}

func promptString(value any) string {
	switch v := value.(type) {
	case string:
		return cleanPrompt(v)
	default:
		return ""
	}
}

func cleanPrompt(value string) string {
	return strings.Join(strings.Fields(strings.TrimSpace(value)), " ")
}
