package audit

import (
	"bytes"
	"fmt"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/evidence"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/store"
)

func Render(s *store.Store, profile string) (string, error) {
	if profile == "" {
		profile = "private"
	}
	if profile != "private" && profile != "shareable" && profile != "client" {
		return "", fmt.Errorf("unknown audit profile %q", profile)
	}
	candidates, err := s.ListCandidates("")
	if err != nil {
		return "", err
	}
	var b bytes.Buffer
	fmt.Fprintf(&b, "# agbox Workflow Audit\n\nGenerated: %s\nProfile: %s\n\n", time.Now().Format(time.RFC3339), profile)
	if len(candidates) == 0 {
		b.WriteString("No workflow skill candidates yet.\n")
		return b.String(), nil
	}
	for _, c := range candidates {
		card, err := evidence.Build(s, c.ID)
		if err != nil {
			return "", err
		}
		fmt.Fprintf(&b, "## %s\n\n", c.Name)
		fmt.Fprintf(&b, "- State: %s\n- Confidence: %s\n- Repeats: %d\n- Projects: %d\n- Sources: %d\n- Privacy: %s\n- Reason: %s\n",
			c.State, c.Confidence, c.EventCount, c.ProjectCount, c.SourceCount, card.Privacy, card.Reason)
		if profile == "private" && len(card.Excerpts) > 0 {
			b.WriteString("\nEvidence excerpts:\n")
			for _, ex := range card.Excerpts {
				fmt.Fprintf(&b, "- %s\n", ex)
			}
		}
		if profile == "shareable" {
			fmt.Fprintf(&b, "\nRule summary: %s\n", summarize(c))
		}
		if profile == "client" {
			fmt.Fprintf(&b, "\nRecommended skill:\n\n%s\n", c.RuleText)
		}
		b.WriteByte('\n')
	}
	return b.String(), nil
}

func summarize(c model.Candidate) string {
	text := strings.TrimSpace(c.RuleText)
	if len(text) > 120 {
		text = text[:117] + "..."
	}
	if text == "" {
		return c.Description
	}
	return text
}
