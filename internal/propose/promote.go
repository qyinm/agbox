package propose

import (
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/store"
)

func PromoteAfterScan(s *store.Store) error {
	candidates, err := s.ListCandidates("")
	if err != nil {
		return err
	}
	now := time.Now()
	for _, c := range candidates {
		next, ok := nextStateAfterScan(c, now)
		if !ok || next == c.State {
			continue
		}
		if err := s.UpdateCandidateMeta(c.ID, store.CandidateMetaUpdate{State: next}); err != nil {
			return err
		}
	}
	return nil
}

func nextStateAfterScan(c model.Candidate, now time.Time) (model.CandidateState, bool) {
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
	}
	return c.State, false
}