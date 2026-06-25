package watcher

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
)

const (
	Label     = "com.agboxhq.watcher"
	PlistName = Label + ".plist"
)

type WatcherStatus struct {
	Label     string
	PlistPath string
	Installed bool
	Running   bool
	PID       int
	LogPath   string
}

func PlistPath(home string) string {
	return filepath.Join(home, "Library", "LaunchAgents", PlistName)
}

func LogPath(home string) string {
	return filepath.Join(home, ".agbox", "watcher.log")
}

func Install(home, agboxBin string) error {
	if strings.TrimSpace(home) == "" {
		return fmt.Errorf("home directory is required")
	}
	if strings.TrimSpace(agboxBin) == "" {
		return fmt.Errorf("agbox binary path is required")
	}
	agboxBin, err := filepath.Abs(agboxBin)
	if err != nil {
		return err
	}
	plistPath := PlistPath(home)
	if err := os.MkdirAll(filepath.Dir(plistPath), 0o755); err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Join(home, ".agbox"), 0o700); err != nil {
		return err
	}
	plist := renderPlist(agboxBin, LogPath(home))
	if err := os.WriteFile(plistPath, []byte(plist), 0o644); err != nil {
		return err
	}
	if shouldManageLaunchd(home) {
		return startAgent(home, plistPath)
	}
	return nil
}

func Stop(home string) error {
	if shouldManageLaunchd(home) {
		_ = stopAgent(home, PlistPath(home))
	}
	return nil
}

func Uninstall(home string) error {
	plistPath := PlistPath(home)
	if shouldManageLaunchd(home) {
		_ = stopAgent(home, plistPath)
	}
	if err := os.Remove(plistPath); err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}

func IsRunning() (bool, error) {
	if runtime.GOOS != "darwin" {
		return false, nil
	}
	pid, err := launchdPID()
	if err != nil {
		return false, err
	}
	return pid > 0, nil
}

func Status(home string) WatcherStatus {
	status := WatcherStatus{
		Label:     Label,
		PlistPath: PlistPath(home),
		LogPath:   LogPath(home),
	}
	if info, err := os.Stat(status.PlistPath); err == nil && !info.IsDir() {
		status.Installed = true
	}
	if shouldManageLaunchd(home) {
		if running, err := IsRunning(); err == nil {
			status.Running = running
		}
		if status.Running {
			if pid, err := launchdPID(); err == nil {
				status.PID = pid
			}
		}
	}
	return status
}

func shouldManageLaunchd(home string) bool {
	if runtime.GOOS != "darwin" {
		return false
	}
	real := loginHomeDir()
	if real == "" {
		return false
	}
	return filepath.Clean(home) == filepath.Clean(real)
}

func loginHomeDir() string {
	if current, err := user.Current(); err == nil && current.HomeDir != "" {
		return current.HomeDir
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return home
}

func renderPlist(agboxBin, logPath string) string {
	var buf bytes.Buffer
	fmt.Fprintf(&buf, `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>%s</string>
	<key>ProgramArguments</key>
	<array>
		<string>%s</string>
		<string>watch</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>StandardOutPath</key>
	<string>%s</string>
	<key>StandardErrorPath</key>
	<string>%s</string>
</dict>
</plist>
`, Label, agboxBin, logPath, logPath)
	return buf.String()
}

func launchdTarget(home, plistPath string) (string, error) {
	uid, err := launchdUID(home)
	if err != nil {
		return "", err
	}
	return fmt.Sprintf("gui/%d/%s", uid, Label), nil
}

func launchdUID(home string) (int, error) {
	if uid := strings.TrimSpace(os.Getenv("AGBOX_LAUNCHD_UID")); uid != "" {
		return strconv.Atoi(uid)
	}
	out, err := exec.Command("id", "-u").Output()
	if err != nil {
		return 0, err
	}
	return strconv.Atoi(strings.TrimSpace(string(out)))
}

func startAgent(home, plistPath string) error {
	target, err := launchdTarget(home, plistPath)
	if err != nil {
		return err
	}
	_ = exec.Command("launchctl", "bootout", target).Run()
	if err := exec.Command("launchctl", "bootstrap", fmt.Sprintf("gui/%d", mustUID(home)), plistPath).Run(); err != nil {
		if err := exec.Command("launchctl", "load", "-w", plistPath).Run(); err != nil {
			return fmt.Errorf("launchctl bootstrap/load: %w", err)
		}
	}
	return nil
}

func stopAgent(home, plistPath string) error {
	target, err := launchdTarget(home, plistPath)
	if err != nil {
		return err
	}
	if err := exec.Command("launchctl", "bootout", target).Run(); err != nil {
		if err := exec.Command("launchctl", "unload", "-w", plistPath).Run(); err != nil {
			return fmt.Errorf("launchctl bootout/unload: %w", err)
		}
	}
	return nil
}

func mustUID(home string) int {
	uid, err := launchdUID(home)
	if err != nil {
		return 0
	}
	return uid
}

func launchdPID() (int, error) {
	if runtime.GOOS != "darwin" {
		return 0, nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return 0, err
	}
	target, err := launchdTarget(home, PlistPath(home))
	if err != nil {
		return 0, err
	}
	out, err := exec.Command("launchctl", "print", target).Output()
	if err != nil {
		return 0, nil
	}
	for _, line := range strings.Split(string(out), "\n") {
		line = strings.TrimSpace(line)
		if strings.HasPrefix(line, "pid = ") {
			pid, err := strconv.Atoi(strings.TrimSpace(strings.TrimPrefix(line, "pid = ")))
			if err != nil {
				return 0, err
			}
			return pid, nil
		}
	}
	return 0, nil
}
