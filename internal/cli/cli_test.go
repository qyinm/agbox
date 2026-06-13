package cli

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/hippoom/agbox/internal/privacy"
)

func TestExecuteEndToEndPromotionLoop(t *testing.T) {
	root := t.TempDir()
	t.Setenv("AGBOX_DB", filepath.Join(root, "agbox.db"))
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chdir(root); err != nil {
		t.Fatal(err)
	}
	defer os.Chdir(wd)

	for i := 0; i < 2; i++ {
		if err := Execute([]string{"capture", "--project", "demo", "Use bun, not npm."}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
			t.Fatal(err)
		}
	}
	var out bytes.Buffer
	if err := Execute([]string{"scan"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	candidateID := "cand_" + privacy.HashSignal(privacy.NormalizeSignal("Use bun, not npm."))[:12]
	if !strings.Contains(out.String(), candidateID) {
		t.Fatalf("scan output %q does not include %s", out.String(), candidateID)
	}
	if err := Execute([]string{"approve", candidateID, "--name", "package-manager-workflow"}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	out.Reset()
	if err := Execute([]string{"export", candidateID, "--dry-run"}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), `"path": "AGENTS.md"`) {
		t.Fatalf("dry-run output = %s", out.String())
	}
	out.Reset()
	if err := Execute([]string{"export", candidateID}, strings.NewReader(""), &out, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(root, "AGENTS.md")); err != nil {
		t.Fatal(err)
	}
	if err := Execute([]string{"manifest", "verify"}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if err := Execute([]string{"impact", candidateID}, strings.NewReader(""), &bytes.Buffer{}, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
}
