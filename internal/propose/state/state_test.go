package state_test

import (
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/propose/state"
)

func TestNextAfterScanProposedTTL(t *testing.T) {
	now := time.Now()
	c := model.Candidate{
		State:      model.CandidateProposed,
		ProposedAt: now.Add(-8 * 24 * time.Hour),
		EventCount: 5,
		Confidence: "high",
	}
	next, ok := state.NextAfterScan(c, now)
	if !ok || next != model.CandidateProposalReady {
		t.Fatalf("NextAfterScan() = %s %v, want proposal_ready true", next, ok)
	}
}

func TestMergeOnScanPreservesFrozenState(t *testing.T) {
	existing := model.Candidate{State: model.CandidateProposed, Version: 2}
	incoming := model.Candidate{State: model.CandidatePending, Version: 0}
	merged := state.MergeOnScan(existing, incoming)
	if merged.State != model.CandidateProposed || merged.Version != 2 {
		t.Fatalf("MergeOnScan() = %+v, want proposed v2", merged)
	}
}

func TestMergeOnScanPreservesReplayLifecycleStates(t *testing.T) {
	for _, st := range []model.CandidateState{
		model.CandidateAppliedOnce,
		model.CandidateSaveSuggested,
		model.CandidateAccepted,
		model.CandidateRejected,
		model.CandidateSnoozed,
	} {
		existing := model.Candidate{State: st, Version: 3}
		incoming := model.Candidate{State: model.CandidatePending, Version: 0}
		merged := state.MergeOnScan(existing, incoming)
		if merged.State != st || merged.Version != 3 {
			t.Fatalf("MergeOnScan(%s) = %+v, want %s v3", st, merged, st)
		}
	}
}

func TestNextAfterScanCooldownRecoveryStillWorks(t *testing.T) {
	now := time.Now()
	cases := []model.Candidate{
		{
			State:      model.CandidateRejected,
			UpdatedAt:  now.Add(-state.RejectCooldown - time.Minute),
			EventCount: 5,
			Confidence: "high",
		},
		{
			State:        model.CandidateSnoozed,
			SnoozedUntil: now.Add(-time.Minute),
			EventCount:   5,
			Confidence:   "high",
		},
	}
	for _, c := range cases {
		next, ok := state.NextAfterScan(c, now)
		if !ok || next != model.CandidateProposalReady {
			t.Fatalf("NextAfterScan(%s) = %s %v, want proposal_ready true", c.State, next, ok)
		}
	}
}
