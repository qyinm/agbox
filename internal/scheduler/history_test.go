package scheduler

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
)

type historyAdapter struct {
	testAdapter
	discover func(session.DiscoveryOptions) ([]session.Source, error)
}

func (a *historyAdapter) DiscoverSourcesWithOptions(opts session.DiscoveryOptions) ([]session.Source, error) {
	return a.discover(opts)
}

func TestReconcileUsesPersistedWindowAndDoesNotQueueOldHistory(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "old.jsonl")
	if err := os.WriteFile(path, []byte("old\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	s, err := store.Open(filepath.Join(dir, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	window := 14 * 24 * time.Hour
	if err := s.SetHistoryWindow(window); err != nil {
		t.Fatal(err)
	}
	now := time.Date(2026, 7, 16, 12, 0, 0, 0, time.UTC)
	var gotWindow time.Duration
	adapter := &historyAdapter{testAdapter: testAdapter{agent: "history"}, discover: func(opts session.DiscoveryOptions) ([]session.Source, error) {
		gotWindow = opts.HistoryWindow
		info, err := os.Stat(path)
		if err != nil {
			return nil, err
		}
		return []session.Source{{Agent: "history", Path: path, RootClass: session.RootActive, SourceID: "src_old", Generation: 1, Size: info.Size(), BaselineOffset: info.Size(), HistoricalEligible: false}}, nil
	}}
	controller := New(s)
	controller.Adapters = []session.Adapter{adapter}
	if _, err := controller.Reconcile(ReconcileOptions{Now: now}); err != nil {
		t.Fatal(err)
	}
	if gotWindow != window {
		t.Fatalf("discovery window = %v, want %v", gotWindow, window)
	}
	if _, err := s.GetIngestionWork("src_old", 1); err == nil {
		t.Fatal("old historical active source was queued")
	}
	cp, err := s.GetIngestionCheckpoint("src_old", 1)
	if err != nil || cp.CommittedOffset != 4 {
		t.Fatalf("old source baseline = %+v, %v", cp, err)
	}

	f, err := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.WriteString("new\n"); err != nil {
		f.Close()
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := controller.Reconcile(ReconcileOptions{Now: now.Add(time.Second), LivePath: path}); err != nil {
		t.Fatal(err)
	}
	work, err := s.GetIngestionWork("src_old", 1)
	if err != nil {
		t.Fatal(err)
	}
	if work.Class != store.WorkLive || work.TargetOffset != 8 {
		t.Fatalf("live append work = %+v", work)
	}
}
