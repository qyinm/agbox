package watcher_test

import (
	"context"
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/session/claude"
	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/watcher"
)

func claudeSamplePath(t *testing.T) string {
	t.Helper()
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	return filepath.Join(filepath.Dir(file), "..", "session", "claude", "testdata", "sample.jsonl")
}

func TestRunIngestsOnStartup(t *testing.T) {
	sample, err := os.ReadFile(claudeSamplePath(t))
	if err != nil {
		t.Fatal(err)
	}

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectRoot := filepath.Join(home, ".claude", "projects", "demo")
	if err := os.MkdirAll(projectRoot, 0o755); err != nil {
		t.Fatal(err)
	}
	srcPath := filepath.Join(projectRoot, "session.jsonl")
	if err := os.WriteFile(srcPath, sample, 0o600); err != nil {
		t.Fatal(err)
	}

	dbPath := filepath.Join(home, "agbox.db")
	s, err := store.Open(dbPath)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	go func() {
		done <- watcher.Run(ctx, s, 50*time.Millisecond)
	}()

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		count, err := s.CountCorrections()
		if err != nil {
			t.Fatal(err)
		}
		if count > 0 {
			cancel()
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	select {
	case err := <-done:
		if err != nil && err != context.Canceled {
			t.Fatalf("watcher.Run() = %v", err)
		}
	case <-time.After(3 * time.Second):
		cancel()
		t.Fatal("timed out waiting for initial ingest")
	}

	count, err := s.CountCorrections()
	if err != nil {
		t.Fatal(err)
	}
	if count == 0 {
		t.Fatal("expected corrections after startup ingest")
	}
}

func TestRunIngestsOnFileChange(t *testing.T) {
	sample, err := os.ReadFile(claudeSamplePath(t))
	if err != nil {
		t.Fatal(err)
	}

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectRoot := filepath.Join(home, ".claude", "projects", "demo")
	if err := os.MkdirAll(projectRoot, 0o755); err != nil {
		t.Fatal(err)
	}
	srcPath := filepath.Join(projectRoot, "session.jsonl")
	if err := os.WriteFile(srcPath, nil, 0o600); err != nil {
		t.Fatal(err)
	}

	dbPath := filepath.Join(home, "agbox.db")
	s, err := store.Open(dbPath)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go func() {
		_ = watcher.Run(ctx, s, 200*time.Millisecond)
	}()

	time.Sleep(300 * time.Millisecond)
	if err := os.WriteFile(srcPath, sample, 0o600); err != nil {
		t.Fatal(err)
	}

	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		count, err := s.CountCorrections()
		if err != nil {
			t.Fatal(err)
		}
		if count > 0 {
			cancel()
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	cancel()
	t.Fatal("timed out waiting for file-change ingest")
}

func TestRunTargetedIngestUsesAdapter(t *testing.T) {
	home := t.TempDir()
	srcPath := filepath.Join(home, "sample.jsonl")
	sample, err := os.ReadFile(claudeSamplePath(t))
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(srcPath, sample, 0o600); err != nil {
		t.Fatal(err)
	}

	s, err := store.Open(filepath.Join(home, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	adapter := claude.New()
	src := session.Source{Agent: "claude", Path: srcPath, Project: "demo"}
	if err := session.IngestSource(s, adapter, src); err != nil {
		t.Fatal(err)
	}
	count, err := s.CountCorrections()
	if err != nil {
		t.Fatal(err)
	}
	if count == 0 {
		t.Fatal("expected targeted ingest to store corrections")
	}
}