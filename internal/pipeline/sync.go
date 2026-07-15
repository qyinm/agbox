package pipeline

import (
	"context"
	"errors"
	"time"

	"github.com/hippoom/agbox/internal/propose"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/scheduler"
	"github.com/hippoom/agbox/internal/store"
)

type BestEffortSyncResult struct {
	Ingested       int
	Warning        error
	AcceptedSkills int
	IngestSkipped  bool
}

// SyncAll turns discovery into durable receipts and waits for their terminal
// commit. Parsing is performed only by the fenced scheduler, including when a
// watcher process already owns the lease.
func SyncAll(s *store.Store) (int, error) {
	n, warning, err := syncIngestion(s, true)
	if err != nil {
		return n, err
	}
	if warning != nil {
		return n, warning
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
	n, warning, err := syncIngestion(s, false)
	result := BestEffortSyncResult{Ingested: n, Warning: warning}
	if err != nil {
		result.Warning = errors.Join(result.Warning, err)
	}
	return finishBestEffortSync(s, result)
}

func syncIngestion(s *store.Store, failFast bool) (int, error, error) {
	before, err := s.CountCorrections()
	if err != nil {
		return 0, nil, err
	}
	controller := scheduler.New(s)
	reconciled, reconcileErr := controller.Reconcile(scheduler.ReconcileOptions{CreateReceipt: true, FailFast: failFast})
	if reconcileErr != nil {
		return 0, reconciled.Warning, reconcileErr
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	done := make(chan error, 1)
	go func() { done <- controller.Run(ctx) }()
	waitErr := scheduler.WaitReceipts(ctx, s, reconciled.Receipts)
	cancel()
	<-done
	after, countErr := s.CountCorrections()
	if countErr != nil {
		return 0, reconciled.Warning, countErr
	}
	if waitErr != nil && failFast {
		return after - before, reconciled.Warning, waitErr
	}
	return after - before, errors.Join(reconciled.Warning, waitErr), nil
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
