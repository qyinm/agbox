package telemetry

import (
	"encoding/json"
	"sort"
	"testing"
)

func keys(v any) []string {
	data, err := json.Marshal(v)
	if err != nil {
		return nil
	}
	var m map[string]any
	if err := json.Unmarshal(data, &m); err != nil {
		return nil
	}
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}

func TestInstallCompletedPayloadUsesVersion(t *testing.T) {
	const testVersion = "1.2.3-test"
	old := Version
	Version = testVersion
	t.Cleanup(func() { Version = old })

	props := installCompletedProps("test-id")
	if props.AgboxVersion != testVersion {
		t.Fatalf("AgboxVersion = %q, want %q", props.AgboxVersion, testVersion)
	}
}

func TestDailyActivePayloadUsesVersion(t *testing.T) {
	const testVersion = "4.5.6-test"
	old := Version
	Version = testVersion
	t.Cleanup(func() { Version = old })

	props := dailyActiveProps("test-id", 3)
	if props.AgboxVersion != testVersion {
		t.Fatalf("AgboxVersion = %q, want %q", props.AgboxVersion, testVersion)
	}
}

func TestVersionDefaultNonEmpty(t *testing.T) {
	if Version == "" {
		t.Fatal("Version must not be empty")
	}
}

func TestInstallCompletedPayloadApp(t *testing.T) {
	props := installCompletedProps("test-id")
	if props.App != AppName {
		t.Fatalf("App = %q, want %q", props.App, AppName)
	}
}

func TestInstallCompletedPayloadKeys(t *testing.T) {
	got := keys(installCompletedProps("test-id"))
	want := []string{"agbox_version", "anonymous_id", "app", "arch", "os_family"}
	if len(got) != len(want) {
		t.Fatalf("keys = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("keys = %v, want %v", got, want)
		}
	}
}

func TestDailyActivePayloadKeys(t *testing.T) {
	got := keys(dailyActiveProps("test-id", 7))
	want := []string{"agbox_version", "anonymous_id", "app", "arch", "os_family", "streak_days"}
	if len(got) != len(want) {
		t.Fatalf("keys = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("keys = %v, want %v", got, want)
		}
	}
}