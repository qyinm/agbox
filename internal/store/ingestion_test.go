package store_test

import (
	"database/sql"
	"errors"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	_ "github.com/mattn/go-sqlite3"

	"github.com/hippoom/agbox/internal/store"
)

func TestOpenCreatesCurrentSchemaOnceAndConcurrentOpenersAgree(t *testing.T) {
	path := filepath.Join(t.TempDir(), "agbox.db")
	const openers = 8
	var wg sync.WaitGroup
	errCh := make(chan error, openers)
	stores := make(chan *store.Store, openers)
	for range openers {
		wg.Add(1)
		go func() {
			defer wg.Done()
			s, err := store.Open(path)
			if err == nil {
				stores <- s
			}
			errCh <- err
		}()
	}
	wg.Wait()
	close(errCh)
	close(stores)
	for err := range errCh {
		if err != nil {
			t.Fatalf("concurrent Open: %v", err)
		}
	}
	for s := range stores {
		if got := s.SchemaGeneration(); got != store.CurrentSchemaGeneration {
			t.Errorf("schema generation = %d, want %d", got, store.CurrentSchemaGeneration)
		}
		if err := s.Close(); err != nil {
			t.Fatal(err)
		}
	}

	s, err := store.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	if s.ResetPerformed() {
		t.Fatal("current schema was reset on reopen")
	}
}

func TestOpenResetsCompleteLegacyDatabaseWithoutBackup(t *testing.T) {
	path := filepath.Join(t.TempDir(), "agbox.db")
	createLegacyDatabase(t, path)

	s, err := store.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	if !s.ResetPerformed() {
		t.Fatal("legacy database was not reported as reset")
	}
	if _, err := os.Stat(path + ".bak"); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("unexpected backup: %v", err)
	}
	if n, err := s.CountCorrections(); err != nil || n != 0 {
		t.Fatalf("corrections after reset = %d, err=%v", n, err)
	}
}

func TestOpenRejectsUnknownSymlinkAndNonRegularTargets(t *testing.T) {
	t.Run("unknown sqlite", func(t *testing.T) {
		path := filepath.Join(t.TempDir(), "custom.db")
		db, err := sql.Open("sqlite3", path)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := db.Exec(`CREATE TABLE unrelated (secret TEXT)`); err != nil {
			t.Fatal(err)
		}
		if err := db.Close(); err != nil {
			t.Fatal(err)
		}
		if _, err := store.Open(path); !errors.Is(err, store.ErrUnknownDatabase) {
			t.Fatalf("Open error = %v, want ErrUnknownDatabase", err)
		}
	})

	t.Run("symlink", func(t *testing.T) {
		dir := t.TempDir()
		target := filepath.Join(dir, "target.db")
		if err := os.WriteFile(target, []byte("do not delete"), 0o600); err != nil {
			t.Fatal(err)
		}
		path := filepath.Join(dir, "agbox.db")
		if err := os.Symlink(target, path); err != nil {
			t.Fatal(err)
		}
		if _, err := store.Open(path); !errors.Is(err, store.ErrUnsafeDatabaseTarget) {
			t.Fatalf("Open error = %v, want ErrUnsafeDatabaseTarget", err)
		}
		got, err := os.ReadFile(target)
		if err != nil || string(got) != "do not delete" {
			t.Fatalf("symlink target changed: %q, %v", got, err)
		}
	})

	t.Run("directory", func(t *testing.T) {
		path := filepath.Join(t.TempDir(), "agbox.db")
		if err := os.Mkdir(path, 0o700); err != nil {
			t.Fatal(err)
		}
		if _, err := store.Open(path); !errors.Is(err, store.ErrUnsafeDatabaseTarget) {
			t.Fatalf("Open error = %v, want ErrUnsafeDatabaseTarget", err)
		}
	})

	t.Run("wal symlink", func(t *testing.T) {
		dir := t.TempDir()
		path := filepath.Join(dir, "agbox.db")
		createLegacyDatabase(t, path)
		target := filepath.Join(dir, "unrelated")
		if err := os.WriteFile(target, []byte("keep"), 0o600); err != nil {
			t.Fatal(err)
		}
		if err := os.Symlink(target, path+"-wal"); err != nil {
			t.Fatal(err)
		}
		if _, err := store.Open(path); !errors.Is(err, store.ErrUnsafeDatabaseTarget) {
			t.Fatalf("Open error = %v, want ErrUnsafeDatabaseTarget", err)
		}
	})
}

func TestIngestionQueueCoalescesTargetsAndSurvivesReopen(t *testing.T) {
	path := filepath.Join(t.TempDir(), "agbox.db")
	s, err := store.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Now().UTC()
	source := store.SourceGeneration{SourceID: "src_opaque", Generation: 3, Agent: "codex", SourceRef: "codex:9b1", State: store.SourceActive, CreatedAt: now}
	if err := s.UpsertSourceGeneration(source); err != nil {
		t.Fatal(err)
	}
	first, err := s.EnqueueIngestionWork(store.EnqueueWork{SourceID: source.SourceID, Generation: source.Generation, Class: store.WorkArchive, TargetOffset: 100, ReceiptID: "receipt-1", Now: now})
	if err != nil {
		t.Fatal(err)
	}
	second, err := s.EnqueueIngestionWork(store.EnqueueWork{SourceID: source.SourceID, Generation: source.Generation, Class: store.WorkLive, TargetOffset: 250, ReceiptID: "receipt-2", Now: now.Add(time.Second)})
	if err != nil {
		t.Fatal(err)
	}
	if first.SourceID != second.SourceID || second.TargetOffset != 250 || second.Class != store.WorkLive || second.State != store.WorkPending {
		t.Fatalf("coalesced work = %+v (first %+v)", second, first)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	s, err = store.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	got, err := s.GetIngestionWork(source.SourceID, source.Generation)
	if err != nil {
		t.Fatal(err)
	}
	if got.TargetOffset != 250 || got.Class != store.WorkLive || got.State != store.WorkPending {
		t.Fatalf("reopened work = %+v", got)
	}
}

func TestLeaseFencingAndCommitRollbackAreAtomic(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	now := time.Now().UTC()
	source := store.SourceGeneration{SourceID: "src_atomic", Generation: 1, Agent: "claude", SourceRef: "claude:4ff", State: store.SourceActive, CreatedAt: now}
	if err := s.UpsertSourceGeneration(source); err != nil {
		t.Fatal(err)
	}
	if _, err := s.EnqueueIngestionWork(store.EnqueueWork{SourceID: source.SourceID, Generation: 1, Class: store.WorkLive, TargetOffset: 90, ReceiptID: "receipt", Now: now}); err != nil {
		t.Fatal(err)
	}
	oldLease, err := s.AcquireSchedulerLease("owner-a", now, time.Second)
	if err != nil {
		t.Fatal(err)
	}
	newLease, err := s.AcquireSchedulerLease("owner-b", now.Add(2*time.Second), time.Second)
	if err != nil {
		t.Fatal(err)
	}
	if newLease.FencingToken <= oldLease.FencingToken {
		t.Fatalf("new fence %d <= old fence %d", newLease.FencingToken, oldLease.FencingToken)
	}

	commit := store.SliceCommit{
		SourceID: source.SourceID, Generation: 1, ExpectedOffset: 0, NextOffset: 90,
		ParserStateVersion: 1, ParserState: []byte(`{"turn":4}`), VisibilityWatermark: 8,
		ReceiptID: "receipt", LeaseOwner: oldLease.OwnerID, FencingToken: oldLease.FencingToken, Now: now.Add(2 * time.Second), Complete: true,
	}
	if err := s.CommitIngestionSlice(commit, nil); !errors.Is(err, store.ErrStaleFence) {
		t.Fatalf("stale commit error = %v, want ErrStaleFence", err)
	}

	commit.LeaseOwner = newLease.OwnerID
	commit.FencingToken = newLease.FencingToken
	boom := errors.New("write failed")
	err = s.CommitIngestionSlice(commit, func(tx *sql.Tx) error {
		_, err := tx.Exec(`INSERT INTO events (id, hash, normalized, source, agent, project, excerpt, raw, raw_stored, created_at) VALUES ('evt_atomic', 'h', 'n', 's', 'a', 'p', 'e', '', 0, '')`)
		if err != nil {
			return err
		}
		return boom
	})
	if !errors.Is(err, boom) {
		t.Fatalf("commit error = %v, want injected failure", err)
	}
	cp, err := s.GetIngestionCheckpoint(source.SourceID, 1)
	if err != nil {
		t.Fatal(err)
	}
	if cp.CommittedOffset != 0 || cp.VisibilityWatermark != 0 || len(cp.ParserState) != 0 {
		t.Fatalf("checkpoint advanced on rollback: %+v", cp)
	}
	events, err := s.ListEvents()
	if err != nil {
		t.Fatal(err)
	}
	if len(events) != 0 {
		t.Fatalf("event survived rollback: %d", len(events))
	}
}

func TestQuarantineAndGenerationGuardedResumeSurviveReopen(t *testing.T) {
	path := filepath.Join(t.TempDir(), "agbox.db")
	s, err := store.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Now().UTC()
	if err := s.UpsertSourceGeneration(store.SourceGeneration{SourceID: "src_q", Generation: 7, Agent: "grok", SourceRef: "grok:222", State: store.SourceActive, CreatedAt: now}); err != nil {
		t.Fatal(err)
	}
	if _, err := s.EnqueueIngestionWork(store.EnqueueWork{SourceID: "src_q", Generation: 7, Class: store.WorkLive, TargetOffset: 18, Now: now}); err != nil {
		t.Fatal(err)
	}
	lease, err := s.AcquireSchedulerLease("owner", now, time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	if err := s.QuarantineSource(store.QuarantineRequest{SourceID: "src_q", Generation: 7, ExpectedOffset: 0, FailureClass: "oversized_signal", LeaseOwner: lease.OwnerID, FencingToken: lease.FencingToken, Now: now}); err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	s, err = store.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	work, err := s.GetIngestionWork("src_q", 7)
	if err != nil || work.State != store.WorkQuarantined || work.FailureClass != "oversized_signal" {
		t.Fatalf("quarantine after reopen = %+v, %v", work, err)
	}
	if err := s.ResumeSource("src_q", 6, now.Add(time.Second)); !errors.Is(err, store.ErrGenerationMismatch) {
		t.Fatalf("wrong generation resume = %v", err)
	}
	if err := s.ResumeSource("src_q", 7, now.Add(time.Second)); err != nil {
		t.Fatal(err)
	}
	work, err = s.GetIngestionWork("src_q", 7)
	if err != nil || work.State != store.WorkPending || work.RetryCount != 0 {
		t.Fatalf("resumed work = %+v, %v", work, err)
	}
}

func createLegacyDatabase(t *testing.T, path string) {
	t.Helper()
	s, err := store.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	db, err := sql.Open("sqlite3", path)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	for _, ddl := range []string{
		`DROP TABLE ingestion_receipts`, `DROP TABLE ingestion_work`, `DROP TABLE ingestion_checkpoints`,
		`DROP TABLE ingestion_sources`, `DROP TABLE scheduler_lease`, `DROP TABLE consumer_visibility`, `DROP TABLE agbox_schema`,
	} {
		if _, err := db.Exec(ddl); err != nil {
			t.Fatal(err)
		}
	}
}
