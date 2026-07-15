package codex

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/session"
)

func TestDiscoverSourcesSeparatesOfficialActiveAndArchiveRoots(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	now := time.Now()
	active := filepath.Join(home, ".codex", "sessions", fmt.Sprintf("%04d", now.Year()), fmt.Sprintf("%02d", int(now.Month())), fmt.Sprintf("%02d", now.Day()), "active.jsonl")
	archive := filepath.Join(home, ".codex", "archived_sessions", fmt.Sprintf("rollout-%04d-%02d-%02d.jsonl", now.Year(), int(now.Month()), now.Day()))
	unrelated := filepath.Join(home, ".codex", "backup", "copied.jsonl")
	for _, path := range []string{active, archive, unrelated} {
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte("{}\n"), 0o600); err != nil {
			t.Fatal(err)
		}
	}

	sources, err := New().DiscoverSources()
	if err != nil {
		t.Fatal(err)
	}
	if len(sources) != 2 {
		t.Fatalf("sources = %d, want active+archive only: %+v", len(sources), sources)
	}
	classes := map[session.RootClass]bool{}
	for _, src := range sources {
		classes[src.RootClass] = true
		if src.Path == unrelated {
			t.Fatal("generic .codex backup was discovered")
		}
	}
	if !classes[session.RootActive] || !classes[session.RootArchive] {
		t.Fatalf("root classes = %v", classes)
	}
}
