package propose

import (
	"time"

	proposestate "github.com/hippoom/agbox/internal/propose/state"
	"github.com/hippoom/agbox/internal/store"
)

func PromoteAfterScan(s *store.Store) error {
	candidates, err := s.ListCandidates("")
	if err != nil {
		return err
	}
	now := time.Now()
	for _, c := range candidates {
		next, ok := proposestate.NextAfterScan(c, now)
		if !ok || next == c.State {
			continue
		}
		if err := s.UpdateCandidateMeta(c.ID, store.CandidateMetaUpdate{State: next}); err != nil {
			return err
		}
	}
	return nil
}