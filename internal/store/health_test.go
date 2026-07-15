package store

import (
	"encoding/json"
	"errors"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestIngestionHealthSnapshotIsPrivacySafeAndActionable(t *testing.T) {
	s, err := Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	now := time.Date(2026, 7, 16, 12, 0, 0, 0, time.UTC)
	rawPath := "/Users/alice/Secret Project/transcript-user-prompt.jsonl"
	if err := s.UpsertSourceGeneration(SourceGeneration{SourceID: rawPath, Generation: 3, Agent: "malicious-agent-secret", SourceRef: rawPath, State: SourceActive, CreatedAt: now, UpdatedAt: now}); err != nil {
		t.Fatal(err)
	}
	if _, err := s.EnqueueIngestionWork(EnqueueWork{SourceID: rawPath, Generation: 3, Class: WorkLive, TargetOffset: 99, Now: now}); err != nil {
		t.Fatal(err)
	}
	lease, err := s.AcquireSchedulerLease("owner", now, time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	if err := s.QuarantineSource(QuarantineRequest{SourceID: rawPath, Generation: 3, ExpectedOffset: 0, FailureClass: "prompt=top-secret", LeaseOwner: lease.OwnerID, FencingToken: lease.FencingToken, Now: now}); err != nil {
		t.Fatal(err)
	}

	health := s.IngestionHealthAt(now.Add(time.Second))
	if health.Version != 1 || health.State != HealthDegraded || len(health.Quarantines) != 1 {
		t.Fatalf("health snapshot = %+v", health)
	}
	q := health.Quarantines[0]
	if !strings.HasPrefix(q.SourceID, "srcop_") || q.Agent != "unknown" || q.FailureCode != "ingestion_failure" {
		t.Fatalf("quarantine projection = %+v", q)
	}
	wantAction := "agbox sources resume " + q.SourceID + " --generation 3"
	if q.NextAction != wantAction {
		t.Fatalf("next action = %q, want %q", q.NextAction, wantAction)
	}
	data, err := json.Marshal(health)
	if err != nil {
		t.Fatal(err)
	}
	for _, secret := range []string{"Secret Project", "transcript-user-prompt", "top-secret", "malicious-agent-secret"} {
		if strings.Contains(string(data), secret) {
			t.Fatalf("health JSON leaked %q: %s", secret, data)
		}
	}
}

func TestIngestionHealthPartialDiagnosticsKeepIndependentFields(t *testing.T) {
	s, err := Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	if _, err := s.db.Exec(`DROP TABLE ingestion_policy`); err != nil {
		t.Fatal(err)
	}
	health := s.IngestionHealthAt(time.Now())
	if health.Version != 1 || health.State != HealthHealthy || health.LiveQueueDepth != 0 {
		t.Fatalf("independent health fields lost: %+v", health)
	}
	found := false
	for _, unavailable := range health.Unavailable {
		if unavailable.Field == "history_window_days" && unavailable.Code == "schema_unavailable" {
			found = true
		}
	}
	if !found {
		t.Fatalf("missing field-scoped diagnostic: %+v", health.Unavailable)
	}
}

func TestOpaqueResumeRequarantinesThenRejectsReplacement(t *testing.T) {
	s, err := Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	now := time.Now().UTC()
	const rawID = "internal-source-key"
	if err := s.UpsertSourceGeneration(SourceGeneration{SourceID: rawID, Generation: 1, Agent: "codex", SourceRef: "/private/source", State: SourceActive, CreatedAt: now}); err != nil {
		t.Fatal(err)
	}
	if _, err := s.EnqueueIngestionWork(EnqueueWork{SourceID: rawID, Generation: 1, Class: WorkLive, TargetOffset: 20, Now: now}); err != nil {
		t.Fatal(err)
	}
	lease, err := s.AcquireSchedulerLease("owner", now, time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	if err := s.QuarantineSource(QuarantineRequest{SourceID: rawID, Generation: 1, ExpectedOffset: 0, FailureClass: "parse_error", LeaseOwner: lease.OwnerID, FencingToken: lease.FencingToken, Now: now}); err != nil {
		t.Fatal(err)
	}
	opaque := s.IngestionHealthAt(now).Quarantines[0].SourceID
	if err := s.ResumeSourceByOpaqueID(opaque, 1, now.Add(time.Second)); err != nil {
		t.Fatal(err)
	}
	if _, err := s.ClaimNextIngestionWork(lease.OwnerID, lease.FencingToken, now.Add(2*time.Second)); err != nil {
		t.Fatal(err)
	}
	if err := s.QuarantineSource(QuarantineRequest{SourceID: rawID, Generation: 1, ExpectedOffset: 0, FailureClass: "parse_error", LeaseOwner: lease.OwnerID, FencingToken: lease.FencingToken, Now: now.Add(2 * time.Second)}); err != nil {
		t.Fatal(err)
	}
	if got := s.IngestionHealthAt(now.Add(3 * time.Second)).Quarantines; len(got) != 1 || got[0].Retries != 1 {
		t.Fatalf("unchanged source was not re-quarantined: %+v", got)
	}
	if err := s.UpsertSourceGeneration(SourceGeneration{SourceID: rawID, Generation: 2, Agent: "codex", SourceRef: "/private/replacement", State: SourceActive, CreatedAt: now.Add(4 * time.Second)}); err != nil {
		t.Fatal(err)
	}
	if err := s.ResumeSourceByOpaqueID(opaque, 1, now.Add(5*time.Second)); !errors.Is(err, ErrGenerationMismatch) {
		t.Fatalf("replacement resume = %v, want generation mismatch", err)
	}
}

func TestWaitingAppendDoesNotDegradeRunnableHealth(t *testing.T) {
	s, err := Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	now := time.Now().UTC()
	if err := s.UpsertSourceGeneration(SourceGeneration{SourceID: "tail", Generation: 1, Agent: "codex", SourceRef: "opaque", State: SourceActive, CreatedAt: now}); err != nil {
		t.Fatal(err)
	}
	if _, err := s.EnqueueIngestionWork(EnqueueWork{SourceID: "tail", Generation: 1, Class: WorkLive, TargetOffset: 20, Now: now}); err != nil {
		t.Fatal(err)
	}
	lease, err := s.AcquireSchedulerLease("owner", now, time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := s.ClaimNextIngestionWork(lease.OwnerID, lease.FencingToken, now); err != nil {
		t.Fatal(err)
	}
	if err := s.CommitIngestionSlice(SliceCommit{SourceID: "tail", Generation: 1, ExpectedOffset: 0, NextOffset: 10,
		ParserStateVersion: 1, ParserState: []byte(`{"t":0}`), VisibilityWatermark: 1,
		LeaseOwner: lease.OwnerID, FencingToken: lease.FencingToken, Now: now, AwaitingAppend: true}, nil); err != nil {
		t.Fatal(err)
	}
	health := s.IngestionHealthAt(now.Add(time.Hour))
	if health.State != HealthHealthy || health.LiveQueueDepth != 0 || health.OldestLiveLagMS != 0 || len(health.Violations) != 0 {
		t.Fatalf("waiting append degraded runnable health: %+v", health)
	}
	if health.Consumer.Completeness != ConsumerIncomplete || health.Consumer.LivePending != 1 {
		t.Fatalf("waiting append disappeared from consumer completeness: %+v", health.Consumer)
	}
}
