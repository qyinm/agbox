package session

import (
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestDiscoverRootRejectsSymlinksAndUsesTrustedSessionTime(t *testing.T) {
	root := t.TempDir()
	recent := filepath.Join(root, "2026", "07", "15", "recent.jsonl")
	old := filepath.Join(root, "2020", "01", "02", "old.jsonl")
	for _, path := range []string{recent, old} {
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte("do not read me"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	outside := filepath.Join(t.TempDir(), "outside.jsonl")
	if err := os.WriteFile(outside, []byte("secret"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(outside, filepath.Join(root, "linked.jsonl")); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(old, time.Now(), time.Now()); err != nil {
		t.Fatal(err)
	}

	sources, err := DiscoverRoot(RootSpec{
		Path: root, Class: RootArchive, Recursive: true,
		Match:       func(rel string, _ os.DirEntry) bool { return filepath.Ext(rel) == ".jsonl" },
		SessionTime: DatePathSessionTime,
	}, DiscoveryOptions{Now: time.Date(2026, 7, 16, 0, 0, 0, 0, time.UTC), HistoryWindow: 90 * 24 * time.Hour, Agent: "test"})
	if err != nil {
		t.Fatal(err)
	}
	if len(sources) != 2 {
		t.Fatalf("sources = %d, want 2 regular files (symlink excluded)", len(sources))
	}
	byName := map[string]Source{}
	for _, src := range sources {
		byName[filepath.Base(src.Path)] = src
	}
	if !byName[filepath.Base(recent)].HistoricalEligible {
		t.Fatal("recent trusted session should be eligible")
	}
	if byName[filepath.Base(old)].HistoricalEligible {
		t.Fatal("old trusted session must remain ineligible despite fresh mtime")
	}
}

func TestDiscoverRootBaselinesUnverifiableActiveSourceAtEOF(t *testing.T) {
	root := t.TempDir()
	path := filepath.Join(root, "project", "session.jsonl")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("existing history"), 0o600); err != nil {
		t.Fatal(err)
	}

	sources, err := DiscoverRoot(RootSpec{
		Path: root, Class: RootActive, Recursive: true,
		Match: func(rel string, _ os.DirEntry) bool { return filepath.Ext(rel) == ".jsonl" },
	}, DiscoveryOptions{Now: time.Now(), HistoryWindow: 90 * 24 * time.Hour, Agent: "test"})
	if err != nil {
		t.Fatal(err)
	}
	if len(sources) != 1 {
		t.Fatalf("sources = %d, want 1", len(sources))
	}
	if sources[0].HistoricalEligible {
		t.Fatal("unverifiable source should not be eligible for catch-up")
	}
	if got, want := sources[0].BaselineOffset, int64(len("existing history")); got != want {
		t.Fatalf("baseline offset = %d, want EOF %d", got, want)
	}
	if err := os.WriteFile(path, []byte("existing history\nnew live record"), 0o600); err != nil {
		t.Fatal(err)
	}
	next, err := DiscoverRoot(RootSpec{
		Path: root, Class: RootActive, Recursive: true,
		Match: func(rel string, _ os.DirEntry) bool { return filepath.Ext(rel) == ".jsonl" },
	}, DiscoveryOptions{Now: time.Now(), HistoryWindow: 90 * 24 * time.Hour, Agent: "test"})
	if err != nil {
		t.Fatal(err)
	}
	reconciled := ReconcileSources(sources, next)
	if got, want := reconciled.Current[0].BaselineOffset, int64(len("existing history")); got != want {
		t.Fatalf("baseline advanced across live append: got %d, want %d", got, want)
	}
	if reconciled.Current[0].Size <= reconciled.Current[0].BaselineOffset {
		t.Fatal("later active growth was not exposed as live bytes")
	}
}

func TestVerifiedOpenRejectsDiscoveryToOpenReplacement(t *testing.T) {
	root := t.TempDir()
	path := filepath.Join(root, "session.jsonl")
	if err := os.WriteFile(path, []byte("first"), 0o600); err != nil {
		t.Fatal(err)
	}
	sources, err := DiscoverRoot(RootSpec{Path: root, Class: RootActive, Match: func(_ string, _ os.DirEntry) bool { return true }}, DiscoveryOptions{Now: time.Now(), Agent: "test"})
	if err != nil || len(sources) != 1 {
		t.Fatalf("discover = %v, %v", sources, err)
	}
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("replacement"), 0o600); err != nil {
		t.Fatal(err)
	}
	f, err := VerifiedOpen(sources[0])
	if err == nil {
		f.Close()
		t.Fatal("VerifiedOpen accepted a replaced file")
	}
	if !errors.Is(err, ErrSourceIdentityChanged) {
		t.Fatalf("VerifiedOpen error = %v, want ErrSourceIdentityChanged", err)
	}
}

func TestReconcileSourcesPreservesRenameAndRotatesOnTruncate(t *testing.T) {
	root := t.TempDir()
	oldPath := filepath.Join(root, "old.jsonl")
	newPath := filepath.Join(root, "new.jsonl")
	if err := os.WriteFile(oldPath, []byte("123456"), 0o600); err != nil {
		t.Fatal(err)
	}
	discover := func() []Source {
		sources, err := DiscoverRoot(RootSpec{Path: root, Class: RootActive, Match: func(_ string, _ os.DirEntry) bool { return true }}, DiscoveryOptions{Now: time.Now(), Agent: "test"})
		if err != nil {
			t.Fatal(err)
		}
		return sources
	}
	initial := ReconcileSources(nil, discover())
	if len(initial.Current) != 1 || initial.Current[0].Generation != 1 {
		t.Fatalf("initial = %+v", initial.Current)
	}
	if err := os.Rename(oldPath, newPath); err != nil {
		t.Fatal(err)
	}
	renamed := ReconcileSources(initial.Current, discover())
	if got := renamed.Current[0].Generation; got != 1 {
		t.Fatalf("rename generation = %d, want 1", got)
	}
	if err := os.Truncate(newPath, 1); err != nil {
		t.Fatal(err)
	}
	truncated := ReconcileSources(renamed.Current, discover())
	if got := truncated.Current[0].Generation; got != 2 {
		t.Fatalf("truncate generation = %d, want 2", got)
	}
	if len(truncated.Replaced) != 1 {
		t.Fatalf("replaced = %d, want 1", len(truncated.Replaced))
	}
	if err := os.Remove(newPath); err != nil {
		t.Fatal(err)
	}
	deleted := ReconcileSources(truncated.Current, discover())
	if len(deleted.Deleted) != 1 {
		t.Fatalf("deleted = %d, want 1", len(deleted.Deleted))
	}
}
