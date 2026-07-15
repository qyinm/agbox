package watcher

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/fsnotify/fsnotify"

	"github.com/hippoom/agbox/internal/scheduler"
	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
)

const DefaultPollInterval = 5 * time.Minute

var allAdapters = session.All

func Run(ctx context.Context, s *store.Store, pollInterval time.Duration) error {
	return RunWithReady(ctx, s, pollInterval, nil)
}

// RunWithReady closes ready only after root watches are registered and the
// startup metadata reconciliation has been durably enqueued. Callers can use
// this as the readiness barrier before considering the watcher installed.
func RunWithReady(ctx context.Context, s *store.Store, pollInterval time.Duration, ready chan<- struct{}) error {
	if pollInterval <= 0 {
		pollInterval = DefaultPollInterval
	}
	fw, err := fsnotify.NewWatcher()
	if err != nil {
		return err
	}
	defer fw.Close()

	adapters := allAdapters()
	state := &watchState{}
	// Watches precede baseline reconciliation, closing the startup append gap.
	if err := state.watchRoots(fw, adapters); err != nil {
		logError("register roots", err)
	}
	controller := scheduler.New(s)
	controller.Adapters = adapters
	if result, err := controller.Reconcile(scheduler.ReconcileOptions{}); err != nil {
		logError("initial reconcile", err)
	} else if result.Warning != nil {
		logError("initial reconcile", result.Warning)
	}
	if err := state.refresh(fw, adapters); err != nil {
		logError("refresh sources", err)
	}
	if ready != nil {
		close(ready)
	}

	workerCtx, stopWorker := context.WithCancel(ctx)
	defer stopWorker()
	workerDone := make(chan error, 1)
	go func() { workerDone <- controller.Run(workerCtx) }()

	pollTicker := time.NewTicker(pollInterval)
	defer pollTicker.Stop()
	for {
		select {
		case <-ctx.Done():
			stopWorker()
			<-workerDone
			return ctx.Err()
		case <-pollTicker.C:
			if result, err := controller.Reconcile(scheduler.ReconcileOptions{}); err != nil {
				logError("poll reconcile", err)
			} else if result.Warning != nil {
				logError("poll reconcile", result.Warning)
			}
			if err := state.refresh(fw, adapters); err != nil {
				logError("refresh sources", err)
			}
		case event, ok := <-fw.Events:
			if !ok {
				stopWorker()
				<-workerDone
				return nil
			}
			if event.Op&(fsnotify.Write|fsnotify.Create|fsnotify.Rename|fsnotify.Remove) == 0 {
				continue
			}
			if result, err := controller.Reconcile(scheduler.ReconcileOptions{LivePath: event.Name}); err != nil {
				logError("event reconcile", err)
			} else if result.Warning != nil {
				logError("event reconcile", result.Warning)
			}
			if event.Op&(fsnotify.Create|fsnotify.Rename|fsnotify.Remove) != 0 {
				if err := state.refresh(fw, adapters); err != nil {
					logError("event refresh", err)
				}
			}
		case err, ok := <-fw.Errors:
			if !ok {
				stopWorker()
				<-workerDone
				return nil
			}
			if err != nil {
				logError("filesystem watch", err)
				if result, recErr := controller.Reconcile(scheduler.ReconcileOptions{}); recErr != nil {
					logError("error reconcile", recErr)
				} else if result.Warning != nil {
					logError("error reconcile", result.Warning)
				}
			}
		}
	}
}

func logError(context string, err error) {
	if err != nil {
		fmt.Fprintf(os.Stderr, "agbox watcher: %s: %v\n", context, err)
	}
}

type watchState struct{ dirs map[string]struct{} }

func (st *watchState) watchRoots(fw *fsnotify.Watcher, adapters []session.Adapter) error {
	next := make(map[string]struct{})
	var errs []error
	for _, adapter := range adapters {
		rooted, ok := adapter.(session.RootedAdapter)
		if !ok || !rooted.Runnable() {
			continue
		}
		for _, spec := range rooted.RootSpecs() {
			if err := addExistingDirs(fw, spec.Path, spec.Recursive, next); err != nil && !errors.Is(err, os.ErrNotExist) {
				errs = append(errs, fmt.Errorf("%s root %s: %w", adapter.Agent(), spec.Path, err))
			}
		}
	}
	if st.dirs == nil {
		st.dirs = make(map[string]struct{})
	}
	for dir := range next {
		st.dirs[dir] = struct{}{}
	}
	return errors.Join(errs...)
}

func addExistingDirs(fw *fsnotify.Watcher, root string, recursive bool, out map[string]struct{}) error {
	info, err := os.Stat(root)
	if err != nil {
		return err
	}
	if !info.IsDir() {
		return nil
	}
	if !recursive {
		if err := fw.Add(root); err != nil {
			return err
		}
		out[root] = struct{}{}
		return nil
	}
	return filepath.WalkDir(root, func(path string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return nil
		}
		if !entry.IsDir() {
			return nil
		}
		if err := fw.Add(path); err != nil {
			return nil
		}
		out[path] = struct{}{}
		return nil
	})
}

func (st *watchState) refresh(fw *fsnotify.Watcher, adapters []session.Adapter) error {
	next := make(map[string]struct{})
	var errs []error
	for _, adapter := range adapters {
		if rooted, ok := adapter.(session.RootedAdapter); ok && !rooted.Runnable() {
			continue
		}
		sources, err := adapter.DiscoverSources()
		if err != nil {
			errs = append(errs, fmt.Errorf("%s: %w", adapter.Agent(), err))
			continue
		}
		for _, src := range sources {
			dir := filepath.Dir(src.Path)
			next[dir] = struct{}{}
			if _, ok := st.dirs[dir]; !ok {
				_ = fw.Add(dir)
			}
		}
		if rooted, ok := adapter.(session.RootedAdapter); ok {
			for _, spec := range rooted.RootSpecs() {
				_ = addExistingDirs(fw, spec.Path, spec.Recursive, next)
			}
		}
	}
	for dir := range st.dirs {
		if _, keep := next[dir]; !keep {
			_ = fw.Remove(dir)
		}
	}
	st.dirs = next
	return errors.Join(errs...)
}
