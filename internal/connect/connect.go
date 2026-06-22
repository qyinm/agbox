package connect

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/fsx"
)

const (
	AgentClaude = "claude"
	AgentCodex  = "codex"
	AgentGrok   = "grok"

	ActionConnect    = "connect"
	ActionDisconnect = "disconnect"

	managedMarker = "AGBOX_MANAGED_HOOK=1"
)

var (
	executablePath = os.Executable
	managedEvents  = []string{"SessionStart", "Stop", "PostToolUse"}
)

type Options struct {
	Command string
	Project bool
}

type Plan struct {
	Action          string   `json:"action"`
	Agent           string   `json:"agent"`
	Path            string   `json:"path"`
	Command         string   `json:"command,omitempty"`
	WillCreate      bool     `json:"will_create"`
	Changed         bool     `json:"changed"`
	BeforeHash      string   `json:"before_hash"`
	AfterHash       string   `json:"after_hash"`
	Operations      []string `json:"operations"`
	Backup          string   `json:"backup,omitempty"`
	UnsupportedTOML string   `json:"unsupported_toml,omitempty"`
	before          []byte
	after           []byte
	existingFile    bool
	requiresCommand bool
}

type Result struct {
	Plan       Plan
	Changed    bool
	BackupPath string
}

type Status struct {
	Agent   string
	State   string
	Path    string
	Command string
	Detail  string
	OK      bool
}

func BuildPlan(agent, action string, opts Options) (Plan, error) {
	agent, err := normalizeAgent(agent)
	if err != nil {
		return Plan{}, err
	}
	action, err = normalizeAction(action)
	if err != nil {
		return Plan{}, err
	}
	path, err := ConfigPath(agent, opts.Project)
	if err != nil {
		return Plan{}, err
	}
	before, exists, err := readConfig(path)
	if err != nil {
		return Plan{}, err
	}
	cfg, err := decodeConfig(before)
	if err != nil {
		return Plan{}, err
	}
	if err := validateHookShape(cfg); err != nil {
		return Plan{}, err
	}
	beforeNorm, err := encodeConfig(cfg)
	if err != nil {
		return Plan{}, err
	}
	plan := Plan{
		Action:       action,
		Agent:        agent,
		Path:         path,
		WillCreate:   !exists,
		BeforeHash:   fsx.HashBytes(before),
		before:       before,
		existingFile: exists,
	}
	switch action {
	case ActionConnect:
		command, err := resolveCommand(opts.Command)
		if err != nil {
			return Plan{}, err
		}
		plan.Command = command
		plan.requiresCommand = true
		removed := removeManagedHooks(cfg)
		addManagedHooks(cfg, agent, command)
		if removed {
			plan.Operations = append(plan.Operations, "replace existing agbox-managed promotion hooks")
		} else {
			plan.Operations = append(plan.Operations, "add agbox-managed promotion hooks")
		}
		if plan.existingFile {
			plan.Operations = append(plan.Operations, "backup existing config before apply")
		}
		if agent == AgentCodex {
			plan.UnsupportedTOML = codexTOMLHookPath()
		}
	case ActionDisconnect:
		removed := removeManagedHooks(cfg)
		if removed {
			plan.Operations = append(plan.Operations, "remove agbox-managed promotion hooks")
			if plan.existingFile {
				plan.Operations = append(plan.Operations, "backup existing config before apply")
			}
		} else {
			plan.Operations = append(plan.Operations, "no agbox-managed hook found")
		}
	default:
		return Plan{}, fmt.Errorf("unsupported action %q", action)
	}
	after, err := encodeConfig(cfg)
	if err != nil {
		return Plan{}, err
	}
	plan.after = after
	plan.AfterHash = fsx.HashBytes(after)
	plan.Changed = !bytes.Equal(beforeNorm, after)
	if !plan.Changed {
		plan.Operations = append(plan.Operations, "no file changes needed")
	}
	return plan, nil
}

func Apply(plan Plan) (Result, error) {
	if err := ValidateApply(plan); err != nil {
		return Result{}, err
	}
	if !plan.Changed {
		return Result{Plan: plan, Changed: false}, nil
	}
	backupPath := ""
	if plan.existingFile {
		path, err := writeBackup(plan.Agent, plan.before)
		if err != nil {
			return Result{}, err
		}
		backupPath = path
		plan.Backup = backupPath
	}
	if err := writeConfig(plan.Path, plan.after); err != nil {
		return Result{}, err
	}
	return Result{Plan: plan, Changed: true, BackupPath: backupPath}, nil
}

func ValidateApply(plan Plan) error {
	if plan.requiresCommand {
		if err := validateExecutable(plan.Command); err != nil {
			return err
		}
	}
	if !plan.Changed {
		return nil
	}
	if info, err := os.Lstat(plan.Path); err == nil && info.Mode()&os.ModeSymlink != 0 {
		return fmt.Errorf("refusing to write through symlink: %s", plan.Path)
	}
	return nil
}

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

func StatusAll() []Status {
	return []Status{
		AgentStatus(AgentClaude, false),
		AgentStatus(AgentCodex, false),
		AgentStatus(AgentGrok, false),
	}
}

func AgentStatus(agent string, project bool) Status {
	agent, err := normalizeAgent(agent)
	if err != nil {
		return Status{Agent: agent, State: "unsupported", Detail: err.Error(), OK: false}
	}
	path, err := ConfigPath(agent, project)
	if err != nil {
		return Status{Agent: agent, State: "error", Detail: err.Error(), OK: false}
	}
	before, exists, err := readConfig(path)
	if err != nil {
		return Status{Agent: agent, Path: path, State: "error", Detail: err.Error(), OK: false}
	}
	cfg, err := decodeConfig(before)
	if err != nil {
		return Status{Agent: agent, Path: path, State: "parse error", Detail: err.Error(), OK: false}
	}
	if err := validateHookShape(cfg); err != nil {
		return Status{Agent: agent, Path: path, State: "parse error", Detail: err.Error(), OK: false}
	}
	commands := managedCommands(cfg)
	if len(commands) == 0 {
		if agent == AgentCodex {
			if toml := codexTOMLHookPath(); toml != "" {
				return Status{Agent: agent, Path: path, State: "unsupported TOML config detected", Detail: toml, OK: true}
			}
		}
		if exists {
			return Status{Agent: agent, Path: path, State: "not configured", Detail: "no agbox-managed hook entry", OK: true}
		}
		return Status{Agent: agent, Path: path, State: "not configured", Detail: "config file does not exist", OK: true}
	}
	for _, command := range commands {
		commandPath := managedCommandPath(command)
		if commandPath == "" {
			return Status{Agent: agent, Path: path, Command: command, State: "stale command", Detail: "could not parse agbox command path", OK: false}
		}
		if err := validateExecutable(commandPath); err != nil {
			return Status{Agent: agent, Path: path, Command: commandPath, State: "unexecutable command", Detail: err.Error(), OK: false}
		}
	}
	return Status{Agent: agent, Path: path, Command: managedCommandPath(commands[0]), State: "connected", OK: true}
}

func normalizeAgent(agent string) (string, error) {
	agent = strings.ToLower(strings.TrimSpace(agent))
	switch agent {
	case AgentClaude, AgentCodex, AgentGrok:
		return agent, nil
	default:
		return "", fmt.Errorf("unsupported agent %q", agent)
	}
}

func normalizeAction(action string) (string, error) {
	action = strings.ToLower(strings.TrimSpace(action))
	switch action {
	case ActionConnect, ActionDisconnect:
		return action, nil
	default:
		return "", fmt.Errorf("unsupported action %q", action)
	}
}

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
	case "SessionStart":
		return managedMarker + " " + shellQuote(command) + " hook propose " + agent + " --event session-start"
	case "Stop":
		return managedMarker + " " + shellQuote(command) + " hook propose " + agent + " --event stop"
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

func resolveCommand(command string) (string, error) {
	if strings.TrimSpace(command) == "" {
		exe, err := executablePath()
		if err != nil {
			return "", err
		}
		if strings.Contains(exe, "go-build") {
			return "", fmt.Errorf("cannot infer stable agbox path from Go temporary executable %s. Install agbox with `go install ./cmd/agbox` or `npm install -g @agboxhq/cli`, then rerun `agbox connect`, or pass --command /absolute/path/to/agbox", exe)
		}
		command = exe
	}
	abs, err := filepath.Abs(command)
	if err != nil {
		return "", err
	}
	if err := validateExecutable(abs); err != nil {
		return "", err
	}
	return abs, nil
}

func validateExecutable(path string) error {
	if strings.TrimSpace(path) == "" {
		return fmt.Errorf("empty command path")
	}
	info, err := os.Stat(path)
	if err != nil {
		return err
	}
	if info.IsDir() {
		return fmt.Errorf("%s is a directory", path)
	}
	if info.Mode()&0o111 == 0 {
		return fmt.Errorf("%s is not executable", path)
	}
	return nil
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