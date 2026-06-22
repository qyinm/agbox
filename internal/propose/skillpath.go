package propose

import (
	"os"
	"path/filepath"
	"strings"
)

func MatchesSkillPath(agent, path string) bool {
	path = strings.TrimSpace(path)
	if path == "" || filepath.Base(path) != "SKILL.md" {
		return false
	}
	slash := filepath.ToSlash(filepath.Clean(path))
	home, _ := os.UserHomeDir()
	switch agent {
	case "claude":
		return strings.Contains(slash, "/.claude/skills/")
	case "codex":
		return strings.Contains(slash, "/.agents/skills/") ||
			strings.HasPrefix(path, filepath.Join(home, ".codex", "skills"))
	case "grok":
		return strings.Contains(slash, "/.grok/skills/") ||
			strings.HasPrefix(path, filepath.Join(home, ".grok", "skills"))
	default:
		return false
	}
}