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
	return BuildAt(s, candidateID, time.Now())
}

func BuildAt(s *store.Store, candidateID string, now time.Time) (Meter, error) {
	c, err := s.GetCandidate(candidateID)
	if err != nil {
		return Meter{}, err
	}
	baseline, applied, window := impactBaseline(s, c, now)
	events, err := s.EventsForCandidate(candidateID)
	if err != nil {
		return Meter{}, err
	}
	corrections, _ := s.CorrectionsForCandidateAt(candidateID, now)
	after := countAfterBaseline(events, corrections, baseline)
	total := c.EventCount
	if c.SourceKind == model.CandidateSourceCorrection || len(corrections) > 0 {
		total = len(corrections)
	}
	before := total - after
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

func impactBaseline(s *store.Store, c model.Candidate, now time.Time) (baseline time.Time, applied bool, window string) {
	if c.State == model.CandidateAccepted && !c.ProposedAt.IsZero() {
		return c.ProposedAt, true, "active-window before acceptance vs after acceptance as of " + now.Format("2006-01-02")
	}
	exp, err := s.LatestExportForCandidate(c.ID)
	if err != nil && err != sql.ErrNoRows {
		return time.Time{}, false, "no applied export yet; impact starts measuring after export"
	}
	if err == nil && exp.Status == model.ExportApplied && !exp.AppliedAt.IsZero() {
		return exp.AppliedAt, true, "active-window before export vs after export as of " + now.Format("2006-01-02")
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
