package connect

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"

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
	executablePath    = os.Executable
	baseManagedEvents = []string{"SessionStart", "Stop", "PostToolUse"}
	allManagedEvents  = []string{"SessionStart", "Stop", "PostToolUse", "UserPromptSubmit"}
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
