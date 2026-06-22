package propose

import (
	"time"

	"github.com/hippoom/agbox/internal/model"
)

const (
	rejectCooldown = 7 * 24 * time.Hour
	snoozeCooldown = 24 * time.Hour
)

func InCooldown(c model.Candidate, now time.Time) bool {
	switch c.State {
	case model.CandidateRejected:
		if c.UpdatedAt.IsZero() {
			return false
		}
		return now.Before(c.UpdatedAt.Add(rejectCooldown))
	case model.CandidateSnoozed:
		if c.SnoozedUntil.IsZero() {
			return now.Before(c.UpdatedAt.Add(snoozeCooldown))
		}
		return now.Before(c.SnoozedUntil)
	default:
		return false
	}
}

func CooldownExpired(c model.Candidate, now time.Time) bool {
	switch c.State {
	case model.CandidateRejected:
		return !c.UpdatedAt.IsZero() && !now.Before(c.UpdatedAt.Add(rejectCooldown))
	case model.CandidateSnoozed:
		if !c.SnoozedUntil.IsZero() {
			return !now.Before(c.SnoozedUntil)
		}
		return !c.UpdatedAt.IsZero() && !now.Before(c.UpdatedAt.Add(snoozeCooldown))
	default:
		return false
	}
}