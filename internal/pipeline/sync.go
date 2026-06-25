package pipeline

import (
	"time"

	"github.com/hippoom/agbox/internal/propose"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
)

type BestEffortSyncResult struct {
	Ingested       int
	Warning        error
	AcceptedSkills int
	IngestSkipped  bool
}

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
	if _, err := propose.ReconcileAcceptedSkills(s); err != nil {
		return n, err
	}
	return n, nil
}

func SyncBestEffort(s *store.Store) (BestEffortSyncResult, error) {
	n, warning := session.IngestAllBestEffort(s)
	result := BestEffortSyncResult{Ingested: n, Warning: warning}
	return finishBestEffortSync(s, result)
}

func finishBestEffortSync(s *store.Store, result BestEffortSyncResult) (BestEffortSyncResult, error) {
	if _, err := scan.Run(s, 2); err != nil {
		return result, err
	}
	if err := propose.PromoteAfterScan(s); err != nil {
		return result, err
	}
	reconcileResult, err := propose.ReconcileAcceptedSkills(s)
	if err != nil {
		return result, err
	}
	result.AcceptedSkills = reconcileResult.Accepted
	return result, nil
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

func SyncBestEffortIfStale(s *store.Store) (BestEffortSyncResult, error) {
	lastSync, err := s.LatestCursorSync()
	if err != nil {
		return BestEffortSyncResult{}, err
	}
	if !lastSync.IsZero() && time.Since(lastSync) < 5*time.Minute {
		return finishBestEffortSync(s, BestEffortSyncResult{IngestSkipped: true})
	}
	return SyncBestEffort(s)
}
