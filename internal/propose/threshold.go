package propose

import (
	"strings"

	"github.com/hippoom/agbox/internal/model"
)

func MeetsThreshold(c model.Candidate) bool {
	if c.EventCount >= 5 {
		return true
	}
	if c.EventCount >= 3 && (c.Confidence == "medium" || c.Confidence == "high") {
		return true
	}
	if c.EventCount >= 2 && hasSemanticKey(c) {
		return true
	}
	return false
}

func hasSemanticKey(c model.Candidate) bool {
	name := strings.ToLower(c.Name)
	for _, prefix := range []string{"package-manager", "pr-format", "api-route"} {
		if strings.HasPrefix(name, prefix) {
			return true
		}
	}
	return false
}