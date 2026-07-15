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
	oldAllAdapters := allAdapters
	defer func() {
		allAdapters = oldAllAdapters
	}()

	var discoverCalls atomic.Int32
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	allAdapters = func() []session.Adapter {
		return []session.Adapter{countingFailingAdapter{calls: &discoverCalls, cancel: cancel}}
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
	if got := discoverCalls.Load(); got < 2 {
		t.Fatalf("discover calls = %d, want at least 2", got)
	}
}

type countingFailingAdapter struct {
	calls  *atomic.Int32
	cancel context.CancelFunc
}

func (a countingFailingAdapter) Agent() string { return "failing" }
func (a countingFailingAdapter) DiscoverSources() ([]session.Source, error) {
	if a.calls.Add(1) >= 3 {
		a.cancel()
	}
	return nil, errors.New("discover failed")
}
func (a countingFailingAdapter) ParseDelta(session.Source, session.Cursor) (session.ParseResult, error) {
	return session.ParseResult{Session: model.Session{}}, nil
}
