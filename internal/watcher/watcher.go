package watcher

import (
	"context"
	"path/filepath"
	"sync"
	"time"

	"github.com/fsnotify/fsnotify"

	"github.com/hippoom/agbox/internal/pipeline"
	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
)

const DefaultPollInterval = 5 * time.Minute

func Run(ctx context.Context, s *store.Store, pollInterval time.Duration) error {
	if pollInterval <= 0 {
		pollInterval = DefaultPollInterval
	}
	if _, err := pipeline.SyncAll(s); err != nil {
		return err
	}

	fw, err := fsnotify.NewWatcher()
	if err != nil {
		return err
	}
	defer fw.Close()

	state := &watchState{}
	if err := state.refresh(fw); err != nil {
		return err
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
			_, _ = pipeline.SyncAll(s)
		})
	}

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-pollTicker.C:
			if _, err := session.IngestAll(s); err != nil {
				return err
			}
			if err := state.refresh(fw); err != nil {
				return err
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
				if err := session.IngestSource(s, adapter, src); err != nil {
					return err
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
	for _, adapter := range session.All() {
		srcs, err := adapter.DiscoverSources()
		if err != nil {
			return err
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
	return nil
}

func (st *watchState) match(path string) (session.Source, session.Adapter, bool) {
	if ref, ok := st.sources[path]; ok {
		return ref.source, ref.adapter, true
	}
	return session.Source{}, nil, false
}