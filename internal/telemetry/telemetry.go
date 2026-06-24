package telemetry

import (
	"crypto/rand"
	"fmt"
	"os"
	"strings"
	"sync"
	"time"
)

var (
	loadStateWithExists = LoadStateWithExists
	saveState           = SaveState
	loadConfig          = LoadConfig
	utcNow              = func() time.Time { return time.Now().UTC() }
	newClient           = func(cfg Config) *Client { return NewClient(cfg) }
)

// Enabled reports whether the user has telemetry turned on (default: true).
// Network capture still requires a configured POSTHOG_API_KEY (see CaptureReady).
func Enabled() bool {
	if envDisabled() {
		return false
	}
	st, exists, err := loadStateWithExists()
	if err != nil {
		return false
	}
	if exists && !st.Enabled {
		return false
	}
	return true
}

// CaptureReady reports whether telemetry can send network events (enabled + POSTHOG_API_KEY).
func CaptureReady() bool {
	return Enabled() && loadConfig().Enabled()
}

// StatusLine is the doctor/status summary for telemetry state.
func StatusLine() string {
	if !Enabled() {
		return "telemetry: off (re-enable via agbox telemetry on)"
	}
	if !CaptureReady() {
		return "telemetry: on (not configured — no network; set POSTHOG_API_KEY in env or ~/.agbox/.env)"
	}
	return "telemetry: on"
}

var captureMu sync.Mutex

func envDisabled() bool {
	v := strings.ToLower(strings.TrimSpace(os.Getenv("AGBOX_TELEMETRY")))
	return v == "0" || v == "false" || v == "no" || v == "off"
}

func OptIn() (State, error) {
	st, _, err := loadStateWithExists()
	if err != nil {
		return State{}, err
	}
	st.Enabled = true
	if st.AnonymousID == "" {
		id, err := NewAnonymousID()
		if err != nil {
			return State{}, err
		}
		st.AnonymousID = id
	}
	if err := saveState(st); err != nil {
		return State{}, err
	}
	return st, nil
}

func OptOut() error {
	st, _, err := loadStateWithExists()
	if err != nil {
		return err
	}
	st.Enabled = false
	return saveState(st)
}

func NewAnonymousID() (string, error) {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", err
	}
	b[6] = (b[6] & 0x0f) | 0x40
	b[8] = (b[8] & 0x3f) | 0x80
	return fmt.Sprintf("%08x-%04x-%04x-%04x-%012x",
		b[0:4], b[4:6], b[6:8], b[8:10], b[10:16]), nil
}

func prepareState() (State, error) {
	if !Enabled() {
		return State{}, nil
	}
	st, exists, err := loadStateWithExists()
	if err != nil {
		return State{}, err
	}
	if st.AnonymousID == "" {
		id, err := NewAnonymousID()
		if err != nil {
			return State{}, err
		}
		st.AnonymousID = id
		st.Enabled = true
		if err := saveState(st); err != nil {
			return State{}, err
		}
	} else if !exists {
		st.Enabled = true
		if err := saveState(st); err != nil {
			return State{}, err
		}
	}
	return st, nil
}

func RecordInstallCompleted() error {
	captureMu.Lock()
	defer captureMu.Unlock()
	return recordInstallCompletedLocked()
}

func recordInstallCompletedLocked() error {
	if !CaptureReady() {
		return nil
	}
	st, err := prepareState()
	if err != nil || st.AnonymousID == "" {
		return err
	}
	if st.InstallCompletedSent {
		return nil
	}
	props, err := marshalProps(installCompletedProps(st.AnonymousID))
	if err != nil {
		return err
	}
	client := newClient(loadConfig())
	if err := client.Capture(EventInstallCompleted, st.AnonymousID, props); err != nil {
		return nil
	}
	st.InstallCompletedSent = true
	return saveState(st)
}

func RecordDailyActive() error {
	captureMu.Lock()
	defer captureMu.Unlock()
	if !CaptureReady() {
		return nil
	}
	if err := recordInstallCompletedLocked(); err != nil {
		return err
	}
	st, err := prepareState()
	if err != nil || st.AnonymousID == "" {
		return err
	}

	today := utcNow().Format("2006-01-02")
	if st.LastActiveDayUTC == today {
		return nil
	}

	streak := 1
	if st.LastActiveDayUTC != "" {
		yesterday := utcNow().AddDate(0, 0, -1).Format("2006-01-02")
		if st.LastActiveDayUTC == yesterday {
			streak = st.CurrentStreakDays + 1
			if streak < 1 {
				streak = 1
			}
		}
	}

	props, err := marshalProps(dailyActiveProps(st.AnonymousID, streak))
	if err != nil {
		return err
	}
	client := newClient(loadConfig())
	if err := client.Capture(EventDailyActive, st.AnonymousID, props); err != nil {
		return nil
	}

	st.LastActiveDayUTC = today
	st.CurrentStreakDays = streak
	return saveState(st)
}

func MaybeRecordDailyActive() {
	done := make(chan struct{})
	go func() {
		defer close(done)
		_ = RecordDailyActive()
	}()
	select {
	case <-done:
	case <-time.After(captureTimeout + 500*time.Millisecond):
	}
}