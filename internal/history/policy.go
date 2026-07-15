package history

import "time"

const DefaultWindow = 90 * 24 * time.Hour

func Cutoff(now time.Time, window time.Duration) time.Time {
	if window <= 0 {
		window = DefaultWindow
	}
	return now.Add(-window)
}

func Active(at, now time.Time, window time.Duration) bool {
	return !at.Before(Cutoff(now, window))
}
