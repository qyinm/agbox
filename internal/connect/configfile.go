package connect

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/fsx"
)

func readConfig(path string) ([]byte, bool, error) {
	data, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return nil, false, nil
	}
	return data, err == nil, err
}

func decodeConfig(data []byte) (map[string]any, error) {
	if strings.TrimSpace(string(data)) == "" {
		return map[string]any{}, nil
	}
	var cfg map[string]any
	if err := json.Unmarshal(data, &cfg); err != nil {
		return nil, err
	}
	if cfg == nil {
		cfg = map[string]any{}
	}
	return cfg, nil
}

func encodeConfig(cfg map[string]any) ([]byte, error) {
	data, err := json.MarshalIndent(canonicalize(cfg), "", "  ")
	if err != nil {
		return nil, err
	}
	return append(data, '\n'), nil
}

func canonicalize(v any) any {
	switch x := v.(type) {
	case map[string]any:
		keys := make([]string, 0, len(x))
		for k := range x {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		out := make(map[string]any, len(x))
		for _, k := range keys {
			out[k] = canonicalize(x[k])
		}
		return out
	case []any:
		out := make([]any, len(x))
		for i, item := range x {
			out[i] = canonicalize(item)
		}
		return out
	default:
		return v
	}
}

func validateHookShape(cfg map[string]any) error {
	hooksAny, ok := cfg["hooks"]
	if !ok || hooksAny == nil {
		return nil
	}
	hooks, ok := hooksAny.(map[string]any)
	if !ok {
		return fmt.Errorf("hooks must be a JSON object")
	}
	for event, eventAny := range hooks {
		if eventAny == nil {
			continue
		}
		groups, ok := eventAny.([]any)
		if !ok {
			return fmt.Errorf("%s hooks must be a JSON array", event)
		}
		for i, groupAny := range groups {
			group, ok := groupAny.(map[string]any)
			if !ok {
				return fmt.Errorf("%s group %d must be a JSON object", event, i)
			}
			handlersAny, ok := group["hooks"]
			if !ok || handlersAny == nil {
				continue
			}
			if _, ok := handlersAny.([]any); !ok {
				return fmt.Errorf("%s group %d hooks must be a JSON array", event, i)
			}
		}
	}
	return nil
}

func writeConfig(path string, data []byte) error {
	if info, err := os.Lstat(path); err == nil && info.Mode()&os.ModeSymlink != 0 {
		return fmt.Errorf("refusing to write through symlink: %s", path)
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	return fsx.AtomicWrite(path, data, 0o644)
}

func writeBackup(agent string, data []byte) (string, error) {
	home, err := agboxHome()
	if err != nil {
		return "", err
	}
	dir := filepath.Join(home, "hooks", "backups")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", err
	}
	path := filepath.Join(dir, fmt.Sprintf("%s-%s.json", agent, time.Now().UTC().Format("20060102T150405.000000000Z")))
	if err := os.WriteFile(path, data, 0o600); err != nil {
		return "", err
	}
	return path, nil
}

func agboxHome() (string, error) {
	if home := os.Getenv("AGBOX_HOME"); strings.TrimSpace(home) != "" {
		return home, nil
	}
	userHome, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(userHome, ".agbox"), nil
}

func codexTOMLHookPath() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	path := filepath.Join(home, ".codex", "config.toml")
	data, err := os.ReadFile(path)
	if err != nil {
		return ""
	}
	text := string(data)
	if strings.Contains(text, "[hooks") || strings.Contains(text, "[[hooks.") {
		return path
	}
	return ""
}