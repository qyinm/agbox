package scan

import (
	"sort"
	"strings"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
)

func clusterFingerprint(e model.Event) string {
	return privacy.HashSignal(string(model.CandidateSourcePromptPattern) + ":" + clusterKey(e))
}

func clusterKey(e model.Event) string {
	if key := SemanticKey(e.Normalized); key != "" {
		return "semantic:" + key
	}
	return "exact:" + e.Hash
}

func eligiblePromptEvent(e model.Event) bool {
	normalized := strings.TrimSpace(e.Normalized)
	if normalized == "" {
		return false
	}
	if isGeneratedProposalText(e.Excerpt) || isGeneratedProposalText(e.Raw) || isGeneratedProposalText(normalized) {
		return false
	}
	if looksLikeStructuredContext(e.Excerpt) || looksLikeStructuredContext(e.Raw) {
		return false
	}
	if looksLikeNormalizedStructuredContext(normalized) {
		return false
	}
	if isAcknowledgement(normalized) || isNumericOnly(normalized) {
		return false
	}
	tokens := strings.Fields(normalized)
	if len(tokens) < 3 && SemanticKey(normalized) == "" {
		return false
	}
	return true
}

// SemanticKey derives a stable semantic clustering key from normalized signal text.
func SemanticKey(normalized string) string {
	tokens := strings.Fields(normalized)
	if len(tokens) == 0 {
		return ""
	}
	if key := packageManagerKey(tokens); key != "" {
		return key
	}
	if hasAll(tokens, "pr", "summary", "tests", "risk") {
		return "pr-format:summary-tests-risk"
	}
	if hasAny(tokens, "route", "routes", "api") && hasAny(tokens, "openapi", "schema", "schemas") {
		return "api-route-openapi-sync"
	}
	return lexicalKey(tokens)
}

func workflowKind(events []model.Event) string {
	for _, e := range events {
		switch SemanticKey(e.Normalized) {
		case "pr-format:summary-tests-risk":
			return "pr-format-workflow"
		case "api-route-openapi-sync":
			return "api-route-openapi-workflow"
		}
		if strings.HasPrefix(SemanticKey(e.Normalized), "package-manager:") {
			return "package-manager-workflow"
		}
	}
	return ""
}

func workflowDescription(events []model.Event) string {
	if kind := workflowKind(events); kind != "" {
		return "Use when this repeated " + strings.ReplaceAll(kind, "-", " ") + " signal appears in agent sessions."
	}
	return "Use when this repeated workflow instruction appears in agent sessions."
}

func packageManagerKey(tokens []string) string {
	for _, preferred := range packageManagers {
		for _, avoided := range packageManagers {
			if preferred == avoided {
				continue
			}
			if preferOver(tokens, preferred, avoided) {
				return "package-manager:" + preferred + "-over-" + avoided
			}
		}
	}
	return ""
}

func preferOver(tokens []string, preferred, avoided string) bool {
	for i, token := range tokens {
		if token != preferred {
			continue
		}
		if nearby(tokens, i+1, "not", avoided) ||
			nearby(tokens, i+1, "never", avoided) ||
			nearby(tokens, i+1, "instead", "of", avoided) {
			return true
		}
	}
	for i, token := range tokens {
		if token == "prefer" && nearby(tokens, i+1, preferred, "over", avoided) {
			return true
		}
	}
	return false
}

func nearby(tokens []string, start int, pattern ...string) bool {
	if start < 0 || start+len(pattern) > len(tokens) {
		return false
	}
	for i, want := range pattern {
		if tokens[start+i] != want {
			return false
		}
	}
	return true
}

func lexicalKey(tokens []string) string {
	seen := map[string]bool{}
	for _, token := range tokens {
		if stopWords[token] || len(token) < 2 {
			continue
		}
		seen[token] = true
	}
	if len(seen) < 3 {
		return ""
	}
	out := make([]string, 0, len(seen))
	for token := range seen {
		out = append(out, token)
	}
	sort.Strings(out)
	if len(out) > 8 {
		out = out[:8]
	}
	return "lexical:" + strings.Join(out, "-")
}

func hasAll(tokens []string, values ...string) bool {
	for _, value := range values {
		if !hasAny(tokens, value) {
			return false
		}
	}
	return true
}

func hasAny(tokens []string, values ...string) bool {
	for _, token := range tokens {
		for _, value := range values {
			if token == value {
				return true
			}
		}
	}
	return false
}

var packageManagers = []string{"bun", "npm", "pnpm", "yarn"}

func isGeneratedProposalText(value string) bool {
	value = strings.ToLower(strings.TrimSpace(value))
	if value == "" {
		return false
	}
	if strings.Contains(value, "generate") &&
		(strings.Contains(value, "hyperpersonalized suggestions") || strings.Contains(value, "hyper personalized suggestions")) {
		return true
	}
	markers := []string{
		"agbox skill proposal instructions",
		"agbox proposal",
		"agbox candidate",
		"should i create a reusable skill",
		"reply yes no or later",
	}
	for _, marker := range markers {
		if strings.Contains(value, marker) {
			return true
		}
	}
	return false
}

func looksLikeStructuredContext(value string) bool {
	value = strings.TrimSpace(strings.ToLower(value))
	if value == "" {
		return false
	}
	if strings.HasPrefix(value, "<environment_context") ||
		strings.HasPrefix(value, "<system") ||
		strings.HasPrefix(value, "<developer") ||
		strings.HasPrefix(value, "<user") {
		return true
	}
	return strings.HasPrefix(value, "<") && strings.Contains(value, "</")
}

func looksLikeNormalizedStructuredContext(normalized string) bool {
	return strings.HasPrefix(normalized, "environment context") ||
		(strings.HasPrefix(normalized, "system") && strings.HasSuffix(normalized, " system")) ||
		(strings.HasPrefix(normalized, "developer") && strings.HasSuffix(normalized, " developer"))
}

func isAcknowledgement(normalized string) bool {
	switch normalized {
	case "ok", "okay", "yes", "no", "thanks", "thank you", "sure", "done", "got it",
		"네", "예", "아니", "응", "ㅇㅇ", "좋아", "확인", "감사합니다", "고마워":
		return true
	default:
		return false
	}
}

func isNumericOnly(normalized string) bool {
	for _, r := range normalized {
		if r == ' ' {
			continue
		}
		if r < '0' || r > '9' {
			return false
		}
	}
	return normalized != ""
}

var stopWords = map[string]bool{
	"a": true, "an": true, "and": true, "are": true, "as": true, "at": true,
	"be": true, "by": true, "for": true, "from": true, "in": true, "into": true,
	"is": true, "it": true, "of": true, "on": true, "or": true, "our": true,
	"please": true, "should": true, "that": true, "the": true, "this": true,
	"to": true, "use": true, "when": true, "with": true, "you": true,
}
