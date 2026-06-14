package impact

import (
	"database/sql"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/store"
)

type Meter struct {
	CandidateID string
	Before      int
	After       int
	Reduction   int
	Confidence  string
	Window      string
}

func Build(s *store.Store, candidateID string) (Meter, error) {
	c, err := s.GetCandidate(candidateID)
	if err != nil {
		return Meter{}, err
	}
	exp, err := s.LatestExportForCandidate(candidateID)
	if err != nil && err != sql.ErrNoRows {
		return Meter{}, err
	}
	applied := err == nil && exp.Status == model.ExportApplied && !exp.AppliedAt.IsZero()
	events, err := s.EventsForCandidate(candidateID)
	if err != nil {
		return Meter{}, err
	}
	after := 0
	if applied {
		for _, e := range events {
			if e.CreatedAt.After(exp.AppliedAt) {
				after++
			}
		}
	}
	before := c.EventCount - after
	reduction := before - after
	if !applied {
		reduction = 0
	}
	if reduction < 0 {
		reduction = 0
	}
	confidence := "low"
	if !applied {
		confidence = "unmeasured"
	}
	if applied && before >= 3 {
		confidence = "medium"
	}
	if applied && before >= 5 && after == 0 {
		confidence = "high"
	}
	window := "no applied export yet; impact starts measuring after export"
	if applied {
		window = "all-time before export vs after export as of " + time.Now().Format("2006-01-02")
	}
	return Meter{
		CandidateID: candidateID,
		Before:      before,
		After:       after,
		Reduction:   reduction,
		Confidence:  confidence,
		Window:      window,
	}, nil
}
