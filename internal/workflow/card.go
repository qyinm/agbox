package workflow

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"

	"github.com/hippoom/agbox/internal/model"
)

const SafetyNote = "Replay injects instructions and context for the current request only; it does not re-run prior commands or create a persistent skill."

var terminalControlSequence = regexp.MustCompile(`(?:\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b[@-_])`)

type Card struct {
	CandidateID     string
	Name            string
	WhenItApplies   string
	ReplayPlan      []string
	EvidenceSummary string
	Confidence      string
	Lifecycle       string
	SafetyNote      string
}

func Build(card model.EvidenceCard) Card {
	c := card.Candidate
	return Card{
		CandidateID:     c.ID,
		Name:            displayName(c),
		WhenItApplies:   whenItApplies(c),
		ReplayPlan:      replayPlan(c),
		EvidenceSummary: evidenceSummary(card),
		Confidence:      c.Confidence,
		Lifecycle:       LifecycleLabel(c.State),
		SafetyNote:      SafetyNote,
	}
}

func Render(card Card) string {
	var b strings.Builder
	fmt.Fprintf(&b, "Recorded Workflow: %s\n", card.Name)
	if card.CandidateID != "" {
		fmt.Fprintf(&b, "ID: %s\n", card.CandidateID)
	}
	fmt.Fprintf(&b, "When it applies: %s\n", card.WhenItApplies)
	if len(card.ReplayPlan) > 0 {
		b.WriteString("Replay plan:\n")
		for i, step := range card.ReplayPlan {
			fmt.Fprintf(&b, "%d. %s\n", i+1, step)
		}
	}
	if card.EvidenceSummary != "" {
		fmt.Fprintf(&b, "Evidence: %s\n", card.EvidenceSummary)
	}
	if card.Confidence != "" {
		fmt.Fprintf(&b, "Confidence: %s\n", card.Confidence)
	}
	if card.Lifecycle != "" {
		fmt.Fprintf(&b, "Lifecycle: %s\n", card.Lifecycle)
	}
	fmt.Fprintf(&b, "Safety: %s\n", card.SafetyNote)
	return b.String()
}

func LifecycleLabel(state model.CandidateState) string {
	switch state {
	case model.CandidatePending:
		return "Recorded"
	case model.CandidateProposalReady:
		return "Ready to replay"
	case model.CandidateProposed:
		return "Suggested"
	case model.CandidateAppliedOnce:
		return "Applied once"
	case model.CandidateSaveSuggested:
		return "Save suggested"
	case model.CandidateAccepted:
		return "Saved for future"
	case model.CandidateSnoozed:
		return "Snoozed"
	case model.CandidateApproved:
		return "Approved"
	case model.CandidateRejected:
		return "Rejected"
	case model.CandidateExported:
		return "Exported"
	default:
		if state == "" {
			return "Recorded"
		}
		return string(state)
	}
}

func displayName(c model.Candidate) string {
	switch {
	case c.SemanticKey == "current-project-analysis" || c.Name == "current-project-analysis-workflow":
		return "Current Project Analysis"
	case strings.HasPrefix(c.SemanticKey, "package-manager:") || c.Name == "package-manager-workflow":
		return "Package Manager Preference"
	case c.SemanticKey == "pr-format:summary-tests-risk" || c.Name == "pr-format-workflow":
		return "PR Summary Format"
	case c.SemanticKey == "api-route-openapi-sync" || c.Name == "api-route-openapi-workflow":
		return "API Route OpenAPI Sync"
	default:
		if s := titleFromSlug(c.Name); s != "" {
			return s
		}
		return "Recorded Workflow"
	}
}

func whenItApplies(c model.Candidate) string {
	switch {
	case c.SemanticKey == "current-project-analysis":
		return "When the user asks to analyze the current project, repository, codebase, or progress."
	case strings.HasPrefix(c.SemanticKey, "package-manager:"):
		preferred, avoided := packageManagers(c.SemanticKey)
		if preferred != "" && avoided != "" {
			return "When work in this project needs package-manager commands and the recorded preference is " + preferred + " over " + avoided + "."
		}
		return "When work in this project needs package-manager commands."
	case c.SemanticKey == "pr-format:summary-tests-risk":
		return "When the user asks for a pull request or change summary."
	case c.SemanticKey == "api-route-openapi-sync":
		return "When the user changes API routes, schemas, or OpenAPI documentation."
	default:
		if c.SourceKind == model.CandidateSourceCorrection {
			return "When the agent is about to repeat a behavior the user corrected before."
		}
		return "When the current request matches this repeated workflow prompt."
	}
}

func replayPlan(c model.Candidate) []string {
	switch {
	case c.SemanticKey == "current-project-analysis":
		return []string{
			"Inspect repository structure, language stack, package metadata, and tests for this request.",
			"Summarize what the project does, key entry points, current state, and notable risks.",
			"Ground conclusions in files or commands inspected during this request.",
		}
	case strings.HasPrefix(c.SemanticKey, "package-manager:"):
		preferred, avoided := packageManagers(c.SemanticKey)
		if preferred != "" && avoided != "" {
			return []string{
				"Inspect lockfiles and package metadata before choosing commands.",
				"Use " + preferred + " instead of " + avoided + " when the project supports it.",
				"Do not repeat prior wrong package-manager commands from the evidence.",
			}
		}
		return []string{
			"Inspect lockfiles and package metadata before choosing commands.",
			"Follow the recorded package-manager preference for this request.",
			"Do not repeat prior wrong package-manager commands from the evidence.",
		}
	case c.SemanticKey == "pr-format:summary-tests-risk":
		return []string{
			"Summarize the change, tests, and risks in separate sections.",
			"Keep the summary concise and grounded in the current diff.",
			"Call out missing verification when tests were not run.",
		}
	case c.SemanticKey == "api-route-openapi-sync":
		return []string{
			"Inspect route handlers and OpenAPI or schema definitions together.",
			"Keep implementation behavior and documented schema changes synchronized.",
			"Mention any route/schema mismatch that remains after this request.",
		}
	default:
		return []string{
			"Compare the current request with the recorded workflow signal.",
			"Apply only the durable instruction that is supported by the evidence.",
			"Avoid inventing steps that are not present in the recorded pattern.",
		}
	}
}

func evidenceSummary(card model.EvidenceCard) string {
	c := card.Candidate
	parts := []string{"repeated " + strconv.Itoa(c.EventCount) + plural(c.EventCount, " time", " times")}
	if c.ProjectCount > 0 {
		parts = append(parts, "across "+strconv.Itoa(c.ProjectCount)+plural(c.ProjectCount, " project", " projects"))
	}
	if c.SourceCount > 0 {
		parts = append(parts, "from "+strconv.Itoa(c.SourceCount)+plural(c.SourceCount, " source", " sources"))
	}
	summary := strings.Join(parts, " ")
	if len(card.Occurrences) > 0 {
		return summary + "; example: " + oneLine(card.Occurrences[0].AgentAction+" => "+card.Occurrences[0].UserCorrection)
	}
	if len(card.Excerpts) > 0 {
		return summary + "; example prompt: " + oneLine(card.Excerpts[0])
	}
	return summary
}

func packageManagers(key string) (preferred, avoided string) {
	const prefix = "package-manager:"
	rest := strings.TrimPrefix(key, prefix)
	parts := strings.Split(rest, "-over-")
	if len(parts) != 2 {
		return "", ""
	}
	return parts[0], parts[1]
}

func titleFromSlug(slug string) string {
	slug = sanitizeDisplayText(slug)
	if slug == "" {
		return ""
	}
	parts := strings.FieldsFunc(slug, func(r rune) bool {
		return r == '-' || r == '_' || r == ':' || r == '/'
	})
	for i, part := range parts {
		if part == "" {
			continue
		}
		parts[i] = strings.ToUpper(part[:1]) + part[1:]
	}
	return strings.Join(parts, " ")
}

func oneLine(value string) string {
	value = strings.Join(strings.Fields(sanitizeDisplayText(value)), " ")
	if len(value) > 180 {
		return strings.TrimSpace(value[:177]) + "..."
	}
	return value
}

func SanitizeDisplayText(value string) string {
	return sanitizeDisplayText(value)
}

func sanitizeDisplayText(value string) string {
	value = terminalControlSequence.ReplaceAllString(value, "")
	value = strings.Map(func(r rune) rune {
		switch {
		case r == '\n' || r == '\t':
			return r
		case r < 0x20 || r == 0x7f:
			return -1
		default:
			return r
		}
	}, value)
	return strings.TrimSpace(value)
}

func plural(n int, singular, plural string) string {
	if n == 1 {
		return singular
	}
	return plural
}
