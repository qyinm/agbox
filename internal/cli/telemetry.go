package cli

import (
	"errors"
	"fmt"
	"io"
	"os"
	"strings"

	"github.com/hippoom/agbox/internal/telemetry"
)

func telemetryEnvDisabled() bool {
	v := strings.ToLower(strings.TrimSpace(os.Getenv("AGBOX_TELEMETRY")))
	return v == "0" || v == "false" || v == "no" || v == "off"
}

func runTelemetry(args []string, stdin io.Reader, stdout io.Writer) error {
	if len(args) == 0 {
		return errors.New("usage: agbox telemetry on|off|status")
	}
	switch args[0] {
	case "status":
		return runTelemetryStatus(stdout)
	case "on":
		return runTelemetryOn(stdout)
	case "off":
		return runTelemetryOff(stdout)
	default:
		return fmt.Errorf("unknown telemetry subcommand %q", args[0])
	}
}

func runTelemetryStatus(stdout io.Writer) error {
	fmt.Fprintln(stdout, telemetry.StatusLine())
	if telemetry.Enabled() {
		fmt.Fprintln(stdout, "Run `agbox telemetry off` to disable.")
		return nil
	}
	if telemetryEnvDisabled() {
		fmt.Fprintln(stdout, "Disabled by AGBOX_TELEMETRY=0.")
	}
	fmt.Fprintln(stdout, "Run `agbox telemetry on` to re-enable.")
	return nil
}

func runTelemetryOn(stdout io.Writer) error {
	printTelemetryDisclosure(stdout)
	if _, err := telemetry.OptIn(); err != nil {
		return err
	}
	_ = telemetry.RecordInstallCompleted()
	fmt.Fprintln(stdout, "telemetry enabled")
	return nil
}

func runTelemetryOff(stdout io.Writer) error {
	if err := telemetry.OptOut(); err != nil {
		return err
	}
	fmt.Fprintln(stdout, "telemetry disabled")
	return nil
}

func printTelemetryDisclosure(w io.Writer) {
	fmt.Fprintln(w, "Anonymous usage telemetry")
	fmt.Fprintln(w, "")
	fmt.Fprintln(w, "agbox sends:")
	fmt.Fprintln(w, "  - agbox_install_completed once (install/version signal)")
	fmt.Fprintln(w, "  - agbox_daily_active at most once per UTC day (includes streak_days)")
	fmt.Fprintln(w, "")
	fmt.Fprintln(w, "Events go to PostHog (maintainer analytics). Your distinct ID is a")
	fmt.Fprintln(w, "random UUID — not your hostname, username, or machine fingerprint.")
	fmt.Fprintln(w, "")
	fmt.Fprintln(w, "Opt out anytime: agbox telemetry off  or  AGBOX_TELEMETRY=0")
	fmt.Fprintln(w, "")
}