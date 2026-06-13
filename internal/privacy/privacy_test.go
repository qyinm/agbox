package privacy

import "testing"

func TestNormalizeSignalCollapsesPunctuationAndCase(t *testing.T) {
	got := NormalizeSignal("Use BUN, not npm!!!")
	want := "use bun not npm"
	if got != want {
		t.Fatalf("NormalizeSignal() = %q, want %q", got, want)
	}
}

func TestRedactRemovesSensitiveValues(t *testing.T) {
	got := Redact("email me at dev@example.com token=super-secret")
	if got == "email me at dev@example.com token=super-secret" {
		t.Fatal("Redact did not change sensitive input")
	}
	if got != "email me at [redacted-email] token=[redacted]" {
		t.Fatalf("Redact() = %q", got)
	}
}
