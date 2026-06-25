package watcher

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"time"

	"github.com/fsnotify/fsnotify"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
)

const DefaultPollInterval = 5 * time.Minute

var (
	ingestAllBestEffort = session.IngestAllBestEffort
	ingestSource        = session.IngestSource
	allAdapters         = session.All
)

func Run(ctx context.Context, s *store.Store, pollInterval time.Duration) error {
	if pollInterval <= 0 {
		pollInterval = DefaultPollInterval
	}
	if _, err := ingestAllBestEffort(s); err != nil {
		logError("initial ingest", err)
	}

	fw, err := fsnotify.NewWatcher()
	if err != nil {
		return err
	}
	defer fw.Close()

	state := &watchState{}
	if err := state.refresh(fw); err != nil {
		logError("refresh sources", err)
	}

	pollTicker := time.NewTicker(pollInterval)
	defer pollTicker.Stop()

	var (
		mu       sync.Mutex
		debounce *time.Timer
	)
	scheduleIngest := func() {
		mu.Lock()
		defer mu.Unlock()
		if debounce != nil {
			debounce.Stop()
		}
		debounce = time.AfterFunc(300*time.Millisecond, func() {
			if _, err := ingestAllBestEffort(s); err != nil {
				logError("debounced ingest", err)
			}
		})
	}
	defer func() {
		mu.Lock()
		defer mu.Unlock()
		if debounce != nil {
			debounce.Stop()
		}
	}()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-pollTicker.C:
			if _, err := ingestAllBestEffort(s); err != nil {
				logError("poll ingest", err)
			}
			if err := state.refresh(fw); err != nil {
				logError("refresh sources", err)
			}
		case event, ok := <-fw.Events:
			if !ok {
				return nil
			}
			if event.Op&(fsnotify.Write|fsnotify.Create|fsnotify.Rename) == 0 {
				continue
			}
			path := event.Name
			if src, adapter, ok := state.match(path); ok {
				if err := ingestSource(s, adapter, src); err != nil {
					logError("source ingest", err)
					scheduleIngest()
				}
				continue
			}
			scheduleIngest()
		case err, ok := <-fw.Errors:
			if !ok {
				return nil
			}
			if err != nil {
				scheduleIngest()
			}
		}
	}
}

func logError(context string, err error) {
	if err == nil {
		return
	}
	fmt.Fprintf(os.Stderr, "agbox watcher: %s: %v\n", context, err)
}

type watchState struct {
	dirs    map[string]struct{}
	sources map[string]sourceRef
}

type sourceRef struct {
	source  session.Source
	adapter session.Adapter
}

func (st *watchState) refresh(fw *fsnotify.Watcher) error {
	nextDirs := make(map[string]struct{})
	nextSources := make(map[string]sourceRef)
	var errs []error
	for _, adapter := range allAdapters() {
		srcs, err := adapter.DiscoverSources()
		if err != nil {
			errs = append(errs, fmt.Errorf("%s: %w", adapter.Agent(), err))
			continue
		}
		for _, src := range srcs {
			nextSources[src.Path] = sourceRef{source: src, adapter: adapter}
			dir := filepath.Dir(src.Path)
			nextDirs[dir] = struct{}{}
		}
	}
	for dir := range nextDirs {
		if _, ok := st.dirs[dir]; ok {
			continue
		}
		if err := fw.Add(dir); err != nil {
			continue
		}
	}
	if st.dirs == nil {
		st.dirs = make(map[string]struct{})
	}
	if st.sources == nil {
		st.sources = make(map[string]sourceRef)
	}
	st.dirs = nextDirs
	st.sources = nextSources
	return errors.Join(errs...)
}

func (st *watchState) match(path string) (session.Source, session.Adapter, bool) {
	if ref, ok := st.sources[path]; ok {
		return ref.source, ref.adapter, true
	}
	return session.Source{}, nil, false
}
