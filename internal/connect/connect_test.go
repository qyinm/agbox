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
	if countManagedHandlers(cfg) != 1 {
		t.Fatalf("managed hook count = %d, want 1; cfg=%#v", countManagedHandlers(cfg), cfg)
	}
	if !strings.Contains(managedCommands(cfg)[0], cmd) {
		t.Fatalf("managed command = %q, want command path %q", managedCommands(cfg)[0], cmd)
	}
	if _, ok := cfg["hooks"].(map[string]any)["PreToolUse"]; !ok {
		t.Fatalf("unrelated hook event not preserved: %#v", cfg)
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
			promptEvent: []any{
				map[string]any{
					"hooks": []any{
						map[string]any{"type": "command", "command": managedCommand(AgentClaude, cmd)},
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

func TestMalformedConfigRefusesPlanAndLeavesBytes(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cmd := fakeExecutable(t, home)
	configPath := filepath.Join(home, ".codex", "hooks.json")
	if err := os.MkdirAll(filepath.Dir(configPath), 0o700); err != nil {
		t.Fatal(err)
	}
	bad := []byte(`{"hooks":`)
	if err := os.WriteFile(configPath, bad, 0o644); err != nil {
		t.Fatal(err)
	}

	if _, err := BuildPlan(AgentCodex, ActionConnect, Options{Command: cmd}); err == nil {
		t.Fatal("BuildPlan() succeeded for malformed JSON")
	}
	if got := mustRead(t, configPath); string(got) != string(bad) {
		t.Fatalf("malformed config changed: %q", string(got))
	}
}

func TestInvalidHookShapeRefusesPlanAndLeavesBytes(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cmd := fakeExecutable(t, home)
	configPath := filepath.Join(home, ".codex", "hooks.json")
	if err := os.MkdirAll(filepath.Dir(configPath), 0o700); err != nil {
		t.Fatal(err)
	}
	invalid := []byte(`{"hooks":{"UserPromptSubmit":{"hooks":[]}}}`)
	if err := os.WriteFile(configPath, invalid, 0o644); err != nil {
		t.Fatal(err)
	}

	if _, err := BuildPlan(AgentCodex, ActionConnect, Options{Command: cmd}); err == nil {
		t.Fatal("BuildPlan() succeeded for invalid hook shape")
	}
	if got := mustRead(t, configPath); string(got) != string(invalid) {
		t.Fatalf("invalid config changed: %q", string(got))
	}
}

func TestAgentStatusDetectsConnectedAndUnexecutableCommand(t *testing.T) {
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

	status := AgentStatus(AgentCodex)
	if status.State != "connected" || !status.OK {
		t.Fatalf("status = %#v, want connected OK", status)
	}
	if err := os.Chmod(cmd, 0o644); err != nil {
		t.Fatal(err)
	}
	status = AgentStatus(AgentCodex)
	if status.State != "unexecutable command" || status.OK {
		t.Fatalf("status = %#v, want unexecutable failure", status)
	}
}

func TestAgentStatusDetectsUnsupportedCodexTOML(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	tomlPath := filepath.Join(home, ".codex", "config.toml")
	if err := os.MkdirAll(filepath.Dir(tomlPath), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(tomlPath, []byte("[hooks]\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	status := AgentStatus(AgentCodex)
	if status.State != "unsupported TOML config detected" || !strings.Contains(status.Detail, "config.toml") {
		t.Fatalf("status = %#v, want unsupported TOML", status)
	}
}

func fakeExecutable(t *testing.T, dir string) string {
	t.Helper()
	path := filepath.Join(dir, "agbox")
	if err := os.WriteFile(path, []byte("#!/bin/sh\nexit 0\n"), 0o755); err != nil {
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
