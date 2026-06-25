package connect

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestBuildPlanDoesNotWriteConfig(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("AGBOX_HOME", filepath.Join(home, ".agbox"))
	cmd := fakeExecutable(t, home)

	plan, err := BuildPlan(AgentCodex, ActionConnect, Options{Command: cmd})
	if err != nil {
		t.Fatal(err)
	}
	if !plan.Changed {
		t.Fatal("connect plan should include a pending config change")
	}
	if _, err := os.Stat(filepath.Join(home, ".codex", "hooks.json")); !os.IsNotExist(err) {
		t.Fatalf("dry plan wrote config file: %v", err)
	}
}

func TestConnectCodexPreservesUnknownFieldsAndIsIdempotent(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("AGBOX_HOME", filepath.Join(home, ".agbox"))
	cmd := fakeExecutable(t, home)
	configPath := filepath.Join(home, ".codex", "hooks.json")
	if err := os.MkdirAll(filepath.Dir(configPath), 0o700); err != nil {
		t.Fatal(err)
	}
	existing := `{
  "custom": true,
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "echo keep"
          }
        ]
      }
    ]
  }
}
`
	if err := os.WriteFile(configPath, []byte(existing), 0o644); err != nil {
		t.Fatal(err)
	}

	plan, err := BuildPlan(AgentCodex, ActionConnect, Options{Command: cmd})
	if err != nil {
		t.Fatal(err)
	}
	first, err := Apply(plan)
	if err != nil {
		t.Fatal(err)
	}
	if !first.Changed || first.BackupPath == "" {
		t.Fatalf("Apply() changed=%t backup=%q, want changed with backup", first.Changed, first.BackupPath)
	}
	plan, err = BuildPlan(AgentCodex, ActionConnect, Options{Command: cmd})
	if err != nil {
		t.Fatal(err)
	}
	second, err := Apply(plan)
	if err != nil {
		t.Fatal(err)
	}
	if second.Changed {
		t.Fatal("second apply should be idempotent")
	}

	cfg := readJSONConfig(t, configPath)
	if cfg["custom"] != true {
		t.Fatalf("unknown top-level field not preserved: %#v", cfg)
	}
	if countManagedHandlers(cfg) < 3 {
		t.Fatalf("managed hook count = %d, want >= 3; cfg=%#v", countManagedHandlers(cfg), cfg)
	}
	if !strings.Contains(managedCommands(cfg)[0], cmd) {
		t.Fatalf("managed command = %q, want command path %q", managedCommands(cfg)[0], cmd)
	}
	if _, ok := cfg["hooks"].(map[string]any)["PreToolUse"]; !ok {
		t.Fatalf("unrelated hook event not preserved: %#v", cfg)
	}
}

func TestConnectCodexAddsPromptReplayHook(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("AGBOX_HOME", filepath.Join(home, ".agbox"))
	cmd := fakeExecutable(t, home)

	plan, err := BuildPlan(AgentCodex, ActionConnect, Options{Command: cmd})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := Apply(plan); err != nil {
		t.Fatal(err)
	}

	cfg := readJSONConfig(t, filepath.Join(home, ".codex", "hooks.json"))
	promptCommands := managedCommandsForEvent(cfg, "UserPromptSubmit")
	if len(promptCommands) != 1 {
		t.Fatalf("prompt replay hook count = %d, want 1; cfg=%#v", len(promptCommands), cfg)
	}
	if !strings.Contains(promptCommands[0], " hook replay codex") {
		t.Fatalf("prompt hook command = %q, want hook replay codex", promptCommands[0])
	}
	if !strings.Contains(strings.Join(managedCommandsForEvent(cfg, "SessionStart"), "\n"), " hook propose codex") {
		t.Fatalf("session hook missing propose command: %#v", cfg)
	}
	if !strings.Contains(strings.Join(managedCommandsForEvent(cfg, "PostToolUse"), "\n"), " hook acknowledge codex") {
		t.Fatalf("post-tool hook missing acknowledge command: %#v", cfg)
	}
}

func TestConnectGrokWritesAgboxHooksFile(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("AGBOX_HOME", filepath.Join(home, ".agbox"))
	cmd := fakeExecutable(t, home)

	plan, err := BuildPlan(AgentGrok, ActionConnect, Options{Command: cmd})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := Apply(plan); err != nil {
		t.Fatal(err)
	}
	cfg := readJSONConfig(t, filepath.Join(home, ".grok", "hooks", "agbox.json"))
	if countManagedHandlers(cfg) < 3 {
		t.Fatalf("managed hook count = %d, want >= 3", countManagedHandlers(cfg))
	}
	if _, ok := cfg["hooks"].(map[string]any)["UserPromptSubmit"]; ok {
		t.Fatalf("grok should not receive unsupported prompt-submit hook: %#v", cfg)
	}
}

func TestDisconnectRemovesOnlyManagedHook(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("AGBOX_HOME", filepath.Join(home, ".agbox"))
	cmd := fakeExecutable(t, home)
	configPath := filepath.Join(home, ".claude", "settings.json")
	if err := os.MkdirAll(filepath.Dir(configPath), 0o700); err != nil {
		t.Fatal(err)
	}
	cfg := map[string]any{
		"hooks": map[string]any{
			"SessionStart": []any{
				map[string]any{
					"hooks": []any{
						map[string]any{"type": "command", "command": managedHookCommand(AgentClaude, cmd, "SessionStart")},
						map[string]any{"type": "command", "command": "echo keep"},
					},
				},
			},
		},
	}
	data, _ := json.MarshalIndent(cfg, "", "  ")
	if err := os.WriteFile(configPath, append(data, '\n'), 0o644); err != nil {
		t.Fatal(err)
	}

	plan, err := BuildPlan(AgentClaude, ActionDisconnect, Options{})
	if err != nil {
		t.Fatal(err)
	}
	result, err := Apply(plan)
	if err != nil {
		t.Fatal(err)
	}
	if !result.Changed {
		t.Fatal("disconnect should change config")
	}
	cfg = readJSONConfig(t, configPath)
	if countManagedHandlers(cfg) != 0 {
		t.Fatalf("managed hook still present: %#v", cfg)
	}
	if !strings.Contains(string(mustRead(t, configPath)), "echo keep") {
		t.Fatalf("unrelated hook was removed: %s", string(mustRead(t, configPath)))
	}
}

func fakeExecutable(t *testing.T, home string) string {
	t.Helper()
	path := filepath.Join(home, "agbox")
	if err := os.WriteFile(path, []byte("#!/bin/sh\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	return path
}

func readJSONConfig(t *testing.T, path string) map[string]any {
	t.Helper()
	var cfg map[string]any
	if err := json.Unmarshal(mustRead(t, path), &cfg); err != nil {
		t.Fatal(err)
	}
	return cfg
}

func mustRead(t *testing.T, path string) []byte {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func countManagedHandlers(cfg map[string]any) int {
	return len(managedCommands(cfg))
}

func managedCommandsForEvent(cfg map[string]any, event string) []string {
	hooks, ok := cfg["hooks"].(map[string]any)
	if !ok {
		return nil
	}
	groups, ok := hooks[event].([]any)
	if !ok {
		return nil
	}
	var out []string
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
	return out
}
