package telemetry

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"

	"github.com/hippoom/agbox/internal/fsx"
)

const stateFileName = "telemetry.json"

type State struct {
	Enabled              bool   `json:"enabled"`
	AnonymousID          string `json:"anonymous_id,omitempty"`
	InstallCompletedSent bool   `json:"install_completed_sent"`
	LastActiveDayUTC     string `json:"last_active_day_utc,omitempty"`
	CurrentStreakDays    int    `json:"current_streak_days"`
}

func StatePath() (string, error) {
	home, err := HomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, stateFileName), nil
}

func LoadState() (State, error) {
	st, _, err := LoadStateWithExists()
	return st, err
}

func LoadStateWithExists() (State, bool, error) {
	path, err := StatePath()
	if err != nil {
		return State{}, false, err
	}
	data, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return State{}, false, nil
	}
	if err != nil {
		return State{}, false, err
	}
	var st State
	if err := json.Unmarshal(data, &st); err != nil {
		return State{}, false, err
	}
	return st, true, nil
}

func SaveState(st State) error {
	path, err := StatePath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	data, err := json.MarshalIndent(st, "", "  ")
	if err != nil {
		return err
	}
	return fsx.AtomicWrite(path, append(data, '\n'), 0o600)
}

func HomeDir() (string, error) {
	if home := strings.TrimSpace(os.Getenv("AGBOX_HOME")); home != "" {
		return home, nil
	}
	if home := strings.TrimSpace(os.Getenv("HOME")); home != "" {
		return filepath.Join(home, ".agbox"), nil
	}
	userHome, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(userHome, ".agbox"), nil
}