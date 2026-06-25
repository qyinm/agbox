package workflow_test

import (
	"strings"
	"testing"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/workflow"
)

func TestBuildCurrentProjectAnalysisCard(t *testing.T) {
	card := workflow.Build(model.EvidenceCard{
		Candidate: model.Candidate{
			ID:           "cand_current",
			Name:         "current-project-analysis-workflow",
			SemanticKey:  "current-project-analysis",
			SourceKind:   model.CandidateSourcePromptPattern,
			State:        model.CandidateProposalReady,
			EventCount:   4,
			ProjectCount: 1,
			SourceCount:  1,
			Confidence:   "high",
		},
		Excerpts: []string{"현재 프로젝트 분석해줘"},
	})

	if card.Name != "Current Project Analysis" {
		t.Fatalf("name = %q", card.Name)
	}
	for _, want := range []string{
		"current project",
		"Inspect repository structure",
		"Summarize what the project does",
		"does not re-run prior commands",
		"Ready to replay",
	} {
		if !strings.Contains(strings.Join(append(card.ReplayPlan, card.WhenItApplies, card.SafetyNote, card.Lifecycle), "\n"), want) {
			t.Fatalf("card missing %q: %+v", want, card)
		}
	}
	rendered := workflow.Render(card)
	for _, bad := range []string{"Promotion", "promotion", "candidate"} {
		if strings.Contains(rendered, bad) {
			t.Fatalf("rendered card leaked %q:\n%s", bad, rendered)
		}
	}
}

func TestBuildPackageManagerCorrectionCard(t *testing.T) {
	card := workflow.Build(model.EvidenceCard{
		Candidate: model.Candidate{
			ID:           "cand_pkg",
			Name:         "package-manager-workflow",
			SemanticKey:  "package-manager:bun-over-npm",
			SourceKind:   model.CandidateSourceCorrection,
			State:        model.CandidateAppliedOnce,
			EventCount:   3,
			ProjectCount: 1,
			SourceCount:  1,
			Confidence:   "medium",
		},
		Occurrences: []model.Occurrence{
			{AgentAction: "ran `npm install`", UserCorrection: "use bun, not npm"},
		},
	})

	rendered := workflow.Render(card)
	for _, want := range []string{
		"Package Manager Preference",
		"bun over npm",
		"Use bun instead of npm",
		"Do not repeat prior wrong package-manager commands",
		"ran `npm install` => use bun, not npm",
		"Applied once",
	} {
		if !strings.Contains(rendered, want) {
			t.Fatalf("rendered card missing %q:\n%s", want, rendered)
		}
	}
}

func TestBuildLexicalFallbackCardIsConservative(t *testing.T) {
	card := workflow.Build(model.EvidenceCard{
		Candidate: model.Candidate{
			ID:           "cand_fallback",
			Name:         "custom-review-format",
			SemanticKey:  "lexical:custom-review-format",
			SourceKind:   model.CandidateSourcePromptPattern,
			State:        model.CandidatePending,
			EventCount:   2,
			ProjectCount: 1,
			SourceCount:  1,
			Confidence:   "low",
		},
		Excerpts: []string{"custom review format"},
	})

	rendered := workflow.Render(card)
	for _, want := range []string{
		"Custom Review Format",
		"Apply only the durable instruction that is supported by the evidence",
		"Avoid inventing steps",
		"Recorded",
	} {
		if !strings.Contains(rendered, want) {
			t.Fatalf("fallback card missing %q:\n%s", want, rendered)
		}
	}
}

func TestLifecycleLabelMapsSavedForFuture(t *testing.T) {
	if got := workflow.LifecycleLabel(model.CandidateAccepted); got != "Saved for future" {
		t.Fatalf("accepted label = %q", got)
	}
	if got := workflow.LifecycleLabel(model.CandidateSaveSuggested); got != "Save suggested" {
		t.Fatalf("save suggested label = %q", got)
	}
}
