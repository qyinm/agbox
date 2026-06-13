package compile

import (
	"fmt"
	"strings"

	"github.com/hippoom/agbox/internal/model"
)

type Artifact struct {
	CandidateID string
	Name        string
	Target      string
	Body        string
}

func Render(c model.Candidate, target string) (Artifact, error) {
	if c.State != model.CandidateApproved && c.State != model.CandidateExported {
		return Artifact{}, fmt.Errorf("candidate %s is %s; approve before compiling", c.ID, c.State)
	}
	target = normalizeTarget(target)
	body := renderBody(c, target)
	return Artifact{CandidateID: c.ID, Name: c.Name, Target: target, Body: body}, nil
}

func normalizeTarget(target string) string {
	switch strings.ToLower(strings.TrimSpace(target)) {
	case "", "agents", "agents-md", "agents.md":
		return "agents-md"
	case "claude", "claude-code", "claude.md":
		return "claude"
	case "codex":
		return "codex"
	case "cursor":
		return "cursor"
	case "cline":
		return "cline"
	default:
		return target
	}
}

func renderBody(c model.Candidate, target string) string {
	header := fmt.Sprintf("---\nname: %s\ndescription: %s\n---\n", c.Name, c.Description)
	rule := strings.TrimSpace(c.RuleText)
	if rule == "" {
		rule = "Follow this workflow when the matching repeated signal appears."
	}
	switch target {
	case "cursor":
		return fmt.Sprintf("%s\n# %s\n\n%s\n", header, c.Name, rule)
	case "cline":
		return fmt.Sprintf("# %s\n\n%s\n", c.Name, rule)
	default:
		return fmt.Sprintf("%s\nWhen this workflow applies:\n\n1. %s\n\nEvidence: repeated %d times. Confidence: %s.\n",
			header, rule, c.EventCount, c.Confidence)
	}
}
