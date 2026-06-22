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
	baseline, applied, window := impactBaseline(s, c)
	events, err := s.EventsForCandidate(candidateID)
	if err != nil {
		return Meter{}, err
	}
	corrections, _ := s.CorrectionsForCandidate(candidateID)
	after := countAfterBaseline(events, corrections, baseline)
	before := c.EventCount - after
	if before < 0 {
		before = 0
	}
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
	return Meter{
		CandidateID: candidateID,
		Before:      before,
		After:       after,
		Reduction:   reduction,
		Confidence:  confidence,
		Window:      window,
	}, nil
}

func impactBaseline(s *store.Store, c model.Candidate) (baseline time.Time, applied bool, window string) {
	if c.State == model.CandidateAccepted && !c.ProposedAt.IsZero() {
		return c.ProposedAt, true, "all-time before acceptance vs after acceptance as of " + time.Now().Format("2006-01-02")
	}
	exp, err := s.LatestExportForCandidate(c.ID)
	if err != nil && err != sql.ErrNoRows {
		return time.Time{}, false, "no applied export yet; impact starts measuring after export"
	}
	if err == nil && exp.Status == model.ExportApplied && !exp.AppliedAt.IsZero() {
		return exp.AppliedAt, true, "all-time before export vs after export as of " + time.Now().Format("2006-01-02")
	}
	if c.State == model.CandidateApproved || c.State == model.CandidateExported {
		return time.Time{}, false, "no applied export yet; impact starts measuring after export"
	}
	return time.Time{}, false, "no applied export yet; impact starts measuring after export"
}

func countAfterBaseline(events []model.Event, corrections []model.Correction, baseline time.Time) int {
	if baseline.IsZero() {
		return 0
	}
	// Candidates link either events or corrections, never both as duplicate signals.
	if len(corrections) > 0 {
		after := 0
		for _, cor := range corrections {
			if cor.CreatedAt.After(baseline) {
				after++
			}
		}
		return after
	}
	after := 0
	for _, e := range events {
		if e.CreatedAt.After(baseline) {
			after++
		}
	}
	return after
}