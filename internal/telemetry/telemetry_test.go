package telemetry

import (
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

type roundTripFunc func(*http.Request) (*http.Response, error)

func (f roundTripFunc) RoundTrip(req *http.Request) (*http.Response, error) {
	return f(req)
}

func setupTestHome(t *testing.T) string {
	t.Helper()
	home := filepath.Join(t.TempDir(), ".agbox")
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("AGBOX_HOME", home)
	t.Setenv("AGBOX_TELEMETRY", "")
	t.Setenv("POSTHOG_API_KEY", "")
	t.Setenv("POSTHOG_HOST", "")
	return home
}

func writeEnv(t *testing.T, home, content string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(home, ".env"), []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
}

func enableOptIn(t *testing.T, home string) State {
	t.Helper()
	st := State{
		Enabled:              true,
		AnonymousID:          "anon-test-uuid",
		InstallCompletedSent: false,
	}
	if err := saveState(st); err != nil {
		t.Fatal(err)
	}
	return st
}

func TestStatusLineCaptureReady(t *testing.T) {
	setupTestHome(t)
	t.Setenv("POSTHOG_API_KEY", "phc_test")
	if StatusLine() != "telemetry: on" {
		t.Fatalf("status = %q", StatusLine())
	}
}

func TestStatusLineNotConfigured(t *testing.T) {
	setupTestHome(t)
	if !strings.Contains(StatusLine(), "not configured") {
		t.Fatalf("status = %q", StatusLine())
	}
}

func TestEnabledDefaultOn(t *testing.T) {
	setupTestHome(t)
	if !Enabled() {
		t.Fatal("expected telemetry enabled by default")
	}
}

func TestCaptureReadyRequiresAPIKey(t *testing.T) {
	setupTestHome(t)
	if CaptureReady() {
		t.Fatal("expected no capture without POSTHOG_API_KEY")
	}
	t.Setenv("POSTHOG_API_KEY", "phc_test")
	if !CaptureReady() {
		t.Fatal("expected capture with API key and default-on")
	}
}

func TestRecordDailyActiveDefaultOnLazyInit(t *testing.T) {
	setupTestHome(t)
	t.Setenv("POSTHOG_API_KEY", "phc_test")

	var events []string
	origNewClient := newClient
	newClient = func(cfg Config) *Client {
		c := NewClient(cfg)
		c.transport = roundTripFunc(func(req *http.Request) (*http.Response, error) {
			data, _ := io.ReadAll(req.Body)
			var body map[string]any
			_ = json.Unmarshal(data, &body)
			if ev, _ := body["event"].(string); ev != "" {
				events = append(events, ev)
			}
			return &http.Response{
				StatusCode: http.StatusOK,
				Body:       io.NopCloser(strings.NewReader("")),
				Header:     make(http.Header),
			}, nil
		})
		return c
	}
	t.Cleanup(func() { newClient = origNewClient })

	if err := RecordDailyActive(); err != nil {
		t.Fatal(err)
	}
	if len(events) != 2 {
		t.Fatalf("events = %v, want [install_completed, daily_active]", events)
	}
	if events[0] != EventInstallCompleted || events[1] != EventDailyActive {
		t.Fatalf("event order = %v", events)
	}
	st, err := LoadState()
	if err != nil {
		t.Fatal(err)
	}
	if st.AnonymousID == "" || !st.Enabled || !st.InstallCompletedSent {
		t.Fatalf("state = %#v", st)
	}
	if err := RecordDailyActive(); err != nil {
		t.Fatal(err)
	}
	if len(events) != 2 {
		t.Fatalf("same-day dedup sent extra events: %v", events)
	}
}

func TestEnabledAfterExplicitOptOut(t *testing.T) {
	home := setupTestHome(t)
	st := State{Enabled: false}
	if err := saveState(st); err != nil {
		t.Fatal(err)
	}
	_ = home
	if Enabled() {
		t.Fatal("expected disabled after explicit opt-out")
	}
}

func TestEnvTelemetryZeroOverrides(t *testing.T) {
	home := setupTestHome(t)
	enableOptIn(t, home)
	t.Setenv("POSTHOG_API_KEY", "phc_test")
	t.Setenv("AGBOX_TELEMETRY", "0")
	if Enabled() {
		t.Fatal("AGBOX_TELEMETRY=0 should disable telemetry")
	}
}

func TestLoadConfigFromEnvFile(t *testing.T) {
	home := setupTestHome(t)
	writeEnv(t, home, "POSTHOG_API_KEY=phc_from_file\nPOSTHOG_HOST=https://eu.i.posthog.com\n")
	cfg := LoadConfig()
	if cfg.APIKey != "phc_from_file" {
		t.Fatalf("api key = %q", cfg.APIKey)
	}
	if cfg.Host != "https://eu.i.posthog.com" {
		t.Fatalf("host = %q", cfg.Host)
	}
}

func TestLoadConfigEnvOverridesFile(t *testing.T) {
	home := setupTestHome(t)
	writeEnv(t, home, "POSTHOG_API_KEY=phc_from_file\n")
	t.Setenv("POSTHOG_API_KEY", "phc_from_env")
	cfg := LoadConfig()
	if cfg.APIKey != "phc_from_env" {
		t.Fatalf("api key = %q", cfg.APIKey)
	}
}

func TestRecordInstallCompletedOnce(t *testing.T) {
	home := setupTestHome(t)
	enableOptIn(t, home)
	t.Setenv("POSTHOG_API_KEY", "phc_test")

	var mu sync.Mutex
	var bodies []map[string]any
	origNewClient := newClient
	newClient = func(cfg Config) *Client {
		c := NewClient(cfg)
		c.transport = roundTripFunc(func(req *http.Request) (*http.Response, error) {
			data, _ := io.ReadAll(req.Body)
			var body map[string]any
			_ = json.Unmarshal(data, &body)
			mu.Lock()
			bodies = append(bodies, body)
			mu.Unlock()
			return &http.Response{
				StatusCode: http.StatusOK,
				Body:       io.NopCloser(strings.NewReader("")),
				Header:     make(http.Header),
			}, nil
		})
		return c
	}
	t.Cleanup(func() { newClient = origNewClient })

	if err := RecordInstallCompleted(); err != nil {
		t.Fatal(err)
	}
	if err := RecordInstallCompleted(); err != nil {
		t.Fatal(err)
	}
	if len(bodies) != 1 {
		t.Fatalf("expected 1 capture, got %d", len(bodies))
	}
	if bodies[0]["event"] != EventInstallCompleted {
		t.Fatalf("event = %v", bodies[0]["event"])
	}
	props, _ := bodies[0]["properties"].(map[string]any)
	if props["$process_person_profile"] != false {
		t.Fatalf("properties = %#v", props)
	}
	st, err := LoadState()
	if err != nil {
		t.Fatal(err)
	}
	if !st.InstallCompletedSent {
		t.Fatal("install_completed_sent should be true")
	}
}

func TestRecordDailyActiveDedupSameDay(t *testing.T) {
	home := setupTestHome(t)
	st := enableOptIn(t, home)
	st.InstallCompletedSent = true
	st.LastActiveDayUTC = utcNow().Format("2006-01-02")
	if err := saveState(st); err != nil {
		t.Fatal(err)
	}
	t.Setenv("POSTHOG_API_KEY", "phc_test")

	var captures int
	origNewClient := newClient
	newClient = func(cfg Config) *Client {
		c := NewClient(cfg)
		c.transport = roundTripFunc(func(req *http.Request) (*http.Response, error) {
			captures++
			return &http.Response{
				StatusCode: http.StatusOK,
				Body:       io.NopCloser(strings.NewReader("")),
				Header:     make(http.Header),
			}, nil
		})
		return c
	}
	t.Cleanup(func() { newClient = origNewClient })

	if err := RecordDailyActive(); err != nil {
		t.Fatal(err)
	}
	if captures != 0 {
		t.Fatalf("expected 0 captures, got %d", captures)
	}
}

func TestRecordDailyActiveStreakIncrement(t *testing.T) {
	home := setupTestHome(t)
	fixed := time.Date(2026, 6, 24, 12, 0, 0, 0, time.UTC)
	origUTC := utcNow
	utcNow = func() time.Time { return fixed }
	t.Cleanup(func() { utcNow = origUTC })

	st := enableOptIn(t, home)
	st.InstallCompletedSent = true
	st.LastActiveDayUTC = "2026-06-23"
	st.CurrentStreakDays = 3
	if err := saveState(st); err != nil {
		t.Fatal(err)
	}
	t.Setenv("POSTHOG_API_KEY", "phc_test")

	var body map[string]any
	origNewClient := newClient
	newClient = func(cfg Config) *Client {
		c := NewClient(cfg)
		c.transport = roundTripFunc(func(req *http.Request) (*http.Response, error) {
			data, _ := io.ReadAll(req.Body)
			_ = json.Unmarshal(data, &body)
			return &http.Response{
				StatusCode: http.StatusOK,
				Body:       io.NopCloser(strings.NewReader("")),
				Header:     make(http.Header),
			}, nil
		})
		return c
	}
	t.Cleanup(func() { newClient = origNewClient })

	if err := RecordDailyActive(); err != nil {
		t.Fatal(err)
	}
	props, _ := body["properties"].(map[string]any)
	if props["streak_days"] != float64(4) {
		t.Fatalf("streak_days = %v", props["streak_days"])
	}
}

func TestRecordDailyActiveStreakReset(t *testing.T) {
	home := setupTestHome(t)
	fixed := time.Date(2026, 6, 24, 12, 0, 0, 0, time.UTC)
	origUTC := utcNow
	utcNow = func() time.Time { return fixed }
	t.Cleanup(func() { utcNow = origUTC })

	st := enableOptIn(t, home)
	st.InstallCompletedSent = true
	st.LastActiveDayUTC = "2026-06-20"
	st.CurrentStreakDays = 5
	if err := saveState(st); err != nil {
		t.Fatal(err)
	}
	t.Setenv("POSTHOG_API_KEY", "phc_test")

	var body map[string]any
	origNewClient := newClient
	newClient = func(cfg Config) *Client {
		c := NewClient(cfg)
		c.transport = roundTripFunc(func(req *http.Request) (*http.Response, error) {
			data, _ := io.ReadAll(req.Body)
			_ = json.Unmarshal(data, &body)
			return &http.Response{
				StatusCode: http.StatusOK,
				Body:       io.NopCloser(strings.NewReader("")),
				Header:     make(http.Header),
			}, nil
		})
		return c
	}
	t.Cleanup(func() { newClient = origNewClient })

	if err := RecordDailyActive(); err != nil {
		t.Fatal(err)
	}
	props, _ := body["properties"].(map[string]any)
	if props["streak_days"] != float64(1) {
		t.Fatalf("streak_days = %v", props["streak_days"])
	}
}

func TestOptOutDisables(t *testing.T) {
	home := setupTestHome(t)
	enableOptIn(t, home)
	t.Setenv("POSTHOG_API_KEY", "phc_test")
	if err := OptOut(); err != nil {
		t.Fatal(err)
	}
	if Enabled() {
		t.Fatal("expected disabled after opt-out")
	}
}