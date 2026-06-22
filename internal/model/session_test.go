package model_test

import (
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/model"
)

func TestOccurrenceSummaryLine(t *testing.T) {
	occ := model.Occurrence{
		AgentAction:    "ran `npm install`",
		UserCorrection: "use bun, not npm",
	}
	got := occ.SummaryLine()
	want := "ran `npm install`  →  use bun, not npm"
	if got != want {
		t.Fatalf("SummaryLine() = %q, want %q", got, want)
	}
}

func TestDrillStepFormat(t *testing.T) {
	step := model.DrillStep{
		TurnIndex: 3,
		Role:      "agent",
		Summary:   "Ran: npm install",
		CreatedAt: time.Date(2026, 6, 20, 10, 0, 0, 0, time.UTC),
	}
	got := step.Format()
	if got == "" {
		t.Fatal("Format() returned empty string")
	}
}