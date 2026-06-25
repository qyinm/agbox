package compile_test

import (
	"strings"
	"testing"

	"github.com/hippoom/agbox/internal/compile"
	"github.com/hippoom/agbox/internal/model"
)

func TestRenderIncludesCandidateIDFrontmatter(t *testing.T) {
	artifact, err := compile.Render(model.Candidate{
		ID:          "cand_compile123",
		Name:        "package-manager-workflow",
		Description: "Use bun over npm",
		RuleText:    "Use bun, not npm.",
		State:       model.CandidateApproved,
		EventCount:  3,
		Confidence:  "high",
	}, "codex")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(artifact.Body, "agbox_candidate_id: cand_compile123") {
		t.Fatalf("body missing candidate id:\n%s", artifact.Body)
	}
}
