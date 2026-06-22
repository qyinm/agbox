package pipeline

import (
	"time"

	"github.com/hippoom/agbox/internal/propose"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
)

func SyncAll(s *store.Store) (int, error) {
	n, err := session.IngestAll(s)
	if err != nil {
		return n, err
	}
	if _, err := scan.Run(s, 2); err != nil {
		return n, err
	}
	if err := propose.PromoteAfterScan(s); err != nil {
		return n, err
	}
	return n, nil
}

func SyncIfStale(s *store.Store) error {
	lastSync, err := s.LatestCursorSync()
	if err != nil {
		return err
	}
	if !lastSync.IsZero() && time.Since(lastSync) < 5*time.Minute {
		return nil
	}
	_, err = SyncAll(s)
	return err
}