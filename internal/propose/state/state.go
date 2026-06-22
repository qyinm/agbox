package state

import (
	"time"

	"github.com/hippoom/agbox/internal/model"
)

const ProposedTTL = 7 * 24 * time.Hour

const (
	RejectCooldown = 7 * 24 * time.Hour
	SnoozeCooldown = 24 * time.Hour
)

// IsFrozen reports candidate states that should not be overwritten by scan upserts.
func IsFrozen(state model.CandidateState) bool {
	switch state {
	case model.CandidateRejected, model.CandidateExported, model.CandidateAccepted,
		model.CandidateApproved, model.CandidateProposed, model.CandidateProposalReady,
		model.CandidateSnoozed:
		return true
	default:
		return false
	}
}

type MergeResult struct {
	State   model.CandidateState
	Version int
}

// MergeOnScan decides state/version when upserting scan results onto an existing candidate.
func MergeOnScan(existing, incoming model.Candidate) MergeResult {
	if existing.State == "" {
		if incoming.State != "" {
			return MergeResult{State: incoming.State, Version: 1}
		}
		return MergeResult{State: model.CandidatePending, Version: 1}
	}
	if IsFrozen(existing.State) {
		return MergeResult{State: existing.State, Version: existing.Version}
	}
	version := existing.Version + 1
	if version <= 0 {
		version = 1
	}
	return MergeResult{State: existing.State, Version: version}
}

func MeetsThreshold(c model.Candidate) bool {
	if c.EventCount >= 5 {
		return true
	}
	if c.EventCount >= 3 && (c.Confidence == "medium" || c.Confidence == "high") {
		return true
	}
	if c.EventCount >= 2 && c.SemanticKey != "" {
		return true
	}
	return false
}

func InCooldown(c model.Candidate, now time.Time) bool {
	switch c.State {
	case model.CandidateRejected:
		if c.UpdatedAt.IsZero() {
			return false
		}
		return now.Before(c.UpdatedAt.Add(RejectCooldown))
	case model.CandidateSnoozed:
		if c.SnoozedUntil.IsZero() {
			return now.Before(c.UpdatedAt.Add(SnoozeCooldown))
		}
		return now.Before(c.SnoozedUntil)
	default:
		return false
	}
}

func CooldownExpired(c model.Candidate, now time.Time) bool {
	switch c.State {
	case model.CandidateRejected:
		return !c.UpdatedAt.IsZero() && !now.Before(c.UpdatedAt.Add(RejectCooldown))
	case model.CandidateSnoozed:
		if !c.SnoozedUntil.IsZero() {
			return !now.Before(c.SnoozedUntil)
		}
		return !c.UpdatedAt.IsZero() && !now.Before(c.UpdatedAt.Add(SnoozeCooldown))
	default:
		return false
	}
}

// NextAfterScan applies promotion, cooldown recovery, and proposed TTL expiry.
func NextAfterScan(c model.Candidate, now time.Time) (model.CandidateState, bool) {
	switch c.State {
	case model.CandidatePending:
		if MeetsThreshold(c) {
			return model.CandidateProposalReady, true
		}
	case model.CandidateRejected, model.CandidateSnoozed:
		if CooldownExpired(c, now) && MeetsThreshold(c) {
			return model.CandidateProposalReady, true
		}
	case model.CandidateProposalReady:
		if !MeetsThreshold(c) {
			return model.CandidatePending, true
		}
	case model.CandidateProposed:
		if !c.ProposedAt.IsZero() && now.After(c.ProposedAt.Add(ProposedTTL)) {
			return model.CandidateProposalReady, true
		}
	}
	return c.State, false
}