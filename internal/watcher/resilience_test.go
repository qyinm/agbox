package watcher

import (
	"context"
	"errors"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
)

type failingAdapter struct{}

func (failingAdapter) Agent() string {
	return "failing"
}

func (failingAdapter) DiscoverSources() ([]session.Source, error) {
	return nil, errors.New("discover failed")
}

func (failingAdapter) ParseDelta(session.Source, session.Cursor) (session.ParseResult, error) {
	return session.ParseResult{Session: model.Session{}}, nil
}

func TestRunKeepsRunningAfterIngestAndDiscoverErrors(t *testing.T) {
	oldIngestAllBestEffort := ingestAllBestEffort
	oldIngestSource := ingestSource
	oldAllAdapters := allAdapters
	defer func() {
		ingestAllBestEffort = oldIngestAllBestEffort
		ingestSource = oldIngestSource
		allAdapters = oldAllAdapters
	}()

	var ingestCalls atomic.Int32
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	ingestAllBestEffort = func(*store.Store) (int, error) {
		if ingestCalls.Add(1) >= 2 {
			cancel()
		}
		return 0, errors.New("ingest failed")
	}
	ingestSource = func(*store.Store, session.Adapter, session.Source) error {
		return errors.New("source ingest failed")
	}
	allAdapters = func() []session.Adapter {
		return []session.Adapter{failingAdapter{}}
	}

	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	err = Run(ctx, s, 10*time.Millisecond)
	if err != context.Canceled {
		t.Fatalf("Run() = %v, want context.Canceled", err)
	}
	if got := ingestCalls.Load(); got < 2 {
		t.Fatalf("ingest calls = %d, want at least 2", got)
	}
}
