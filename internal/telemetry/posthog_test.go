package telemetry

import "testing"

func TestPosthogCaptureURLRequiresHTTPS(t *testing.T) {
	for _, host := range []string{
		"http://us.i.posthog.com",
		"ftp://us.i.posthog.com",
		"not-a-url",
		"https://evil.example.com",
	} {
		if _, err := posthogCaptureURL(host); err == nil {
			t.Fatalf("expected error for host %q", host)
		}
	}
	got, err := posthogCaptureURL("https://us.i.posthog.com")
	if err != nil {
		t.Fatal(err)
	}
	if got != "https://us.i.posthog.com/capture/" {
		t.Fatalf("url = %q", got)
	}
}