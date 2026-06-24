package telemetry

import (
	"encoding/json"
	"runtime"
)

const (
	AppName               = "agbox"
	EventInstallCompleted = "agbox_install_completed"
	EventDailyActive      = "agbox_daily_active"
)

type InstallCompletedProps struct {
	App          string `json:"app"`
	AgboxVersion string `json:"agbox_version"`
	OSFamily     string `json:"os_family"`
	Arch         string `json:"arch"`
	AnonymousID  string `json:"anonymous_id"`
}

type DailyActiveProps struct {
	App          string `json:"app"`
	AgboxVersion string `json:"agbox_version"`
	OSFamily     string `json:"os_family"`
	Arch         string `json:"arch"`
	AnonymousID  string `json:"anonymous_id"`
	StreakDays   int    `json:"streak_days"`
}

func baseProps(anonymousID string) (string, string, string) {
	return Version, runtime.GOOS, runtime.GOARCH
}

func installCompletedProps(anonymousID string) InstallCompletedProps {
	version, osFamily, arch := baseProps(anonymousID)
	return InstallCompletedProps{
		App:          AppName,
		AgboxVersion: version,
		OSFamily:     osFamily,
		Arch:         arch,
		AnonymousID:  anonymousID,
	}
}

func dailyActiveProps(anonymousID string, streakDays int) DailyActiveProps {
	version, osFamily, arch := baseProps(anonymousID)
	return DailyActiveProps{
		App:          AppName,
		AgboxVersion: version,
		OSFamily:     osFamily,
		Arch:         arch,
		AnonymousID:  anonymousID,
		StreakDays:   streakDays,
	}
}

func marshalProps(v any) (map[string]any, error) {
	data, err := json.Marshal(v)
	if err != nil {
		return nil, err
	}
	var props map[string]any
	if err := json.Unmarshal(data, &props); err != nil {
		return nil, err
	}
	return props, nil
}