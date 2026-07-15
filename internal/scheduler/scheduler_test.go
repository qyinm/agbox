package scheduler

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
)

type testAdapter struct {
	agent   string
	sources []session.Source
	parse   func(session.Source, session.Cursor) (session.ParseResult, error)
}

func (a *testAdapter) Agent() string                              { return a.agent }
func (a *testAdapter) DiscoverSources() ([]session.Source, error) { return a.sources, nil }
func (a *testAdapter) ParseDelta(src session.Source, cur session.Cursor) (session.ParseResult, error) {
	return a.parse(src, cur)
}

func TestConcurrentControllersParseWithOneOwner(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "source.jsonl")
	if err := os.WriteFile(path, []byte("record\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	s1, err := store.Open(filepath.Join(dir, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s1.Close()
	s2, err := store.Open(filepath.Join(dir, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s2.Close()
	var parses atomic.Int32
	adapter := &testAdapter{agent: "test", sources: []session.Source{{
		Agent: "test", Path: path, Project: "demo", RootClass: session.RootActive,
		SourceID: "src_one", Generation: 1, Size: 7, ModTime: time.Now(), HistoricalEligible: true,
	}}, parse: func(src session.Source, cur session.Cursor) (session.ParseResult, error) {
		parses.Add(1)
		time.Sleep(25 * time.Millisecond)
		return parsedTo(src, cur, src.Size), nil
	}}
	c1, c2 := New(s1), New(s2)
	c1.Adapters = []session.Adapter{adapter}
	c2.Adapters = []session.Adapter{adapter}
	result, err := c1.Reconcile(ReconcileOptions{CreateReceipt: true})
	if err != nil {
		t.Fatal(err)
	}
	var wg sync.WaitGroup
	wg.Add(2)
	for _, c := range []*Controller{c1, c2} {
		go func(c *Controller) { defer wg.Done(); _, _, _ = c.ProcessOne(context.Background()) }(c)
	}
	wg.Wait()
	if got := parses.Load(); got != 1 {
		t.Fatalf("parse calls = %d, want exactly 1", got)
	}
	if err := WaitReceipts(context.Background(), s1, result.Receipts); err != nil {
		t.Fatal(err)
	}
}

func TestLiveWorkPreemptsArchiveAndCoalescesSignals(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	archivePath := filepath.Join(dir, "archive.jsonl")
	livePath := filepath.Join(dir, "live.jsonl")
	for _, path := range []string{archivePath, livePath} {
		if err := os.WriteFile(path, []byte("x\n"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	for _, src := range []store.SourceGeneration{
		{SourceID: "src_archive", Generation: 1, Agent: "test", SourceRef: archivePath, State: store.SourceActive},
		{SourceID: "src_live", Generation: 1, Agent: "test", SourceRef: livePath, State: store.SourceActive},
	} {
		if err := s.UpsertSourceGeneration(src); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := s.EnqueueIngestionWork(store.EnqueueWork{SourceID: "src_archive", Generation: 1, Class: store.WorkArchive, TargetOffset: 2}); err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 10_000; i++ {
		if _, err := s.EnqueueIngestionWork(store.EnqueueWork{SourceID: "src_live", Generation: 1, Class: store.WorkLive, TargetOffset: 2}); err != nil {
			t.Fatal(err)
		}
	}
	var first string
	adapter := &testAdapter{agent: "test", parse: func(src session.Source, cur session.Cursor) (session.ParseResult, error) {
		if first == "" {
			first = filepath.Base(src.Path)
		}
		return parsedTo(src, cur, src.Size), nil
	}}
	c := New(s)
	c.Adapters = []session.Adapter{adapter}
	worked, _, err := c.ProcessOne(context.Background())
	if err != nil || !worked {
		t.Fatalf("ProcessOne = %v, %v", worked, err)
	}
	if first != "live.jsonl" {
		t.Fatalf("first processed = %q, want live.jsonl", first)
	}
	work, err := s.GetIngestionWork("src_live", 1)
	if err != nil {
		t.Fatal(err)
	}
	if work.TargetOffset != 2 || work.State != store.WorkComplete {
		t.Fatalf("coalesced work = %+v", work)
	}
}

func TestQuarantineIsolatesSourceAndNextSourceRuns(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	badPath := filepath.Join(dir, "bad.jsonl")
	goodPath := filepath.Join(dir, "good.jsonl")
	for _, path := range []string{badPath, goodPath} {
		if err := os.WriteFile(path, []byte("x\n"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	for _, src := range []store.SourceGeneration{
		{SourceID: "a_bad", Generation: 1, Agent: "test", SourceRef: badPath, State: store.SourceActive},
		{SourceID: "b_good", Generation: 1, Agent: "test", SourceRef: goodPath, State: store.SourceActive},
	} {
		if err := s.UpsertSourceGeneration(src); err != nil {
			t.Fatal(err)
		}
		if _, err := s.EnqueueIngestionWork(store.EnqueueWork{SourceID: src.SourceID, Generation: 1, Class: store.WorkLive, TargetOffset: 2}); err != nil {
			t.Fatal(err)
		}
	}
	adapter := &testAdapter{agent: "test", parse: func(src session.Source, cur session.Cursor) (session.ParseResult, error) {
		if filepath.Base(src.Path) == "bad.jsonl" {
			return session.ParseResult{}, errors.New("bad source")
		}
		return parsedTo(src, cur, src.Size), nil
	}}
	c := New(s)
	c.Adapters = []session.Adapter{adapter}
	if worked, _, err := c.ProcessOne(context.Background()); !worked || err == nil {
		t.Fatalf("bad source result = %v, %v", worked, err)
	}
	if worked, _, err := c.ProcessOne(context.Background()); !worked || err != nil {
		t.Fatalf("good source result = %v, %v", worked, err)
	}
	bad, _ := s.GetIngestionWork("a_bad", 1)
	good, _ := s.GetIngestionWork("b_good", 1)
	if bad.State != store.WorkQuarantined || good.State != store.WorkComplete {
		t.Fatalf("bad=%s good=%s", bad.State, good.State)
	}
}

func TestRestartRecoversClaimFromExpiredFence(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "source.jsonl")
	_ = os.WriteFile(path, []byte("x\n"), 0o600)
	s, err := store.Open(filepath.Join(dir, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	if err := s.UpsertSourceGeneration(store.SourceGeneration{SourceID: "src_restart", Generation: 1, Agent: "test", SourceRef: path, State: store.SourceActive}); err != nil {
		t.Fatal(err)
	}
	if _, err := s.EnqueueIngestionWork(store.EnqueueWork{SourceID: "src_restart", Generation: 1, Class: store.WorkLive, TargetOffset: 2}); err != nil {
		t.Fatal(err)
	}
	past := time.Now().Add(-time.Minute)
	lease, err := s.AcquireSchedulerLease("dead", past, time.Second)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := s.ClaimNextIngestionWork("dead", lease.FencingToken, past); err != nil {
		t.Fatal(err)
	}
	adapter := &testAdapter{agent: "test", parse: func(src session.Source, cur session.Cursor) (session.ParseResult, error) {
		return parsedTo(src, cur, src.Size), nil
	}}
	c := New(s)
	c.Adapters = []session.Adapter{adapter}
	if worked, _, err := c.ProcessOne(context.Background()); !worked || err != nil {
		t.Fatalf("recovered result = %v, %v", worked, err)
	}
}

func parsedTo(src session.Source, cur session.Cursor, next int64) session.ParseResult {
	now := time.Now()
	return session.ParseResult{Session: model.Session{ID: "ses_" + src.SourceID, Agent: src.Agent, Project: src.Project,
		SourcePath: src.Path, SourceHash: "hash", StartedAt: now, UpdatedAt: now}, NewOffset: next,
		NewHash: "hash", ParserStateVersion: 1, ParserState: []byte(`{"t":0}`)}
}
