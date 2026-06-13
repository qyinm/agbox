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
	events, err := s.EventsForCandidate(candidateID)
	if err != nil {
		return Meter{}, err
	}
	after := 0
	if !exp.AppliedAt.IsZero() {
		for _, e := range events {
			if e.CreatedAt.After(exp.AppliedAt) {
				after++
			}
		}
	}
	before := c.EventCount - after
	reduction := before - after
	if reduction < 0 {
		reduction = 0
	}
	confidence := "low"
	if exp.Status == model.ExportApplied && before >= 3 {
		confidence = "medium"
	}
	if exp.Status == model.ExportApplied && before >= 5 && after == 0 {
		confidence = "high"
	}
	return Meter{
		CandidateID: candidateID,
		Before:      before,
		After:       after,
		Reduction:   reduction,
		Confidence:  confidence,
		Window:      "all-time before export vs after export as of " + time.Now().Format("2006-01-02"),
	}, nil
}
