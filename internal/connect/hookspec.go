package connect

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

func ConfigPath(agent string, project bool) (string, error) {
	agent, err := normalizeAgent(agent)
	if err != nil {
		return "", err
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	switch agent {
	case AgentClaude:
		return filepath.Join(home, ".claude", "settings.json"), nil
	case AgentCodex:
		return filepath.Join(home, ".codex", "hooks.json"), nil
	case AgentGrok:
		if project {
			wd, err := os.Getwd()
			if err != nil {
				return "", err
			}
			return filepath.Join(wd, ".grok", "hooks", "agbox.json"), nil
		}
		return filepath.Join(home, ".grok", "hooks", "agbox.json"), nil
	default:
		return "", fmt.Errorf("unsupported agent %q", agent)
	}
}

func addManagedHooks(cfg map[string]any, agent, command string) {
	hooks := ensureObject(cfg, "hooks")
	for _, event := range managedEvents {
		groups, _ := hooks[event].([]any)
		group := managedHookGroup(agent, command, event)
		hooks[event] = append(groups, group)
	}
}

func managedHookGroup(agent, command, event string) map[string]any {
	handler := map[string]any{
		"type":    "command",
		"command": managedHookCommand(agent, command, event),
		"timeout": 5,
	}
	if agent == AgentCodex && event == "SessionStart" {
		handler["statusMessage"] = "Checking agbox skill candidates"
	}
	group := map[string]any{"hooks": []any{handler}}
	switch event {
	case "SessionStart":
		if agent == AgentClaude || agent == AgentCodex {
			group["matcher"] = "startup|resume"
		}
	case "PostToolUse":
		group["matcher"] = "Write|Edit"
	}
	return group
}

func managedHookCommand(agent, command, event string) string {
	switch event {
	case "SessionStart", "Stop":
		return managedMarker + " " + shellQuote(command) + " hook propose " + agent
	case "PostToolUse":
		return managedMarker + " " + shellQuote(command) + " hook acknowledge " + agent
	default:
		return managedMarker + " " + shellQuote(command) + " hook propose " + agent
	}
}

func removeManagedHooks(cfg map[string]any) bool {
	hooks, ok := cfg["hooks"].(map[string]any)
	if !ok {
		return false
	}
	removed := false
	for _, event := range managedEvents {
		groups, ok := hooks[event].([]any)
		if !ok {
			continue
		}
		cleanedGroups := make([]any, 0, len(groups))
		for _, groupAny := range groups {
			group, ok := groupAny.(map[string]any)
			if !ok {
				cleanedGroups = append(cleanedGroups, groupAny)
				continue
			}
			handlers, ok := group["hooks"].([]any)
			if !ok {
				cleanedGroups = append(cleanedGroups, groupAny)
				continue
			}
			cleanedHandlers := make([]any, 0, len(handlers))
			for _, handler := range handlers {
				if isManagedHandler(handler) {
					removed = true
					continue
				}
				cleanedHandlers = append(cleanedHandlers, handler)
			}
			if len(cleanedHandlers) == 0 {
				continue
			}
			group["hooks"] = cleanedHandlers
			cleanedGroups = append(cleanedGroups, group)
		}
		if len(cleanedGroups) == 0 {
			delete(hooks, event)
		} else {
			hooks[event] = cleanedGroups
		}
	}
	return removed
}

func managedCommands(cfg map[string]any) []string {
	hooks, ok := cfg["hooks"].(map[string]any)
	if !ok {
		return nil
	}
	var out []string
	for _, event := range managedEvents {
		groups, ok := hooks[event].([]any)
		if !ok {
			continue
		}
		for _, groupAny := range groups {
			group, ok := groupAny.(map[string]any)
			if !ok {
				continue
			}
			handlers, ok := group["hooks"].([]any)
			if !ok {
				continue
			}
			for _, handlerAny := range handlers {
				handler, ok := handlerAny.(map[string]any)
				if !ok {
					continue
				}
				command, _ := handler["command"].(string)
				if strings.Contains(command, managedMarker) {
					out = append(out, command)
				}
			}
		}
	}
	return out
}

func isManagedHandler(handlerAny any) bool {
	handler, ok := handlerAny.(map[string]any)
	if !ok {
		return false
	}
	command, _ := handler["command"].(string)
	return strings.Contains(command, managedMarker)
}

func ensureObject(parent map[string]any, key string) map[string]any {
	if existing, ok := parent[key].(map[string]any); ok {
		return existing
	}
	child := map[string]any{}
	parent[key] = child
	return child
}

func managedCommandPath(command string) string {
	idx := strings.Index(command, managedMarker)
	if idx < 0 {
		return ""
	}
	rest := strings.TrimSpace(command[idx+len(managedMarker):])
	if rest == "" {
		return ""
	}
	if rest[0] != '\'' {
		fields := strings.Fields(rest)
		if len(fields) == 0 {
			return ""
		}
		return fields[0]
	}
	var b strings.Builder
	for i := 1; i < len(rest); i++ {
		if rest[i] == '\'' {
			return b.String()
		}
		b.WriteByte(rest[i])
	}
	return ""
}

func shellQuote(s string) string {
	return "'" + strings.ReplaceAll(s, "'", "'\\''") + "'"
}