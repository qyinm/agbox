package store_test

import (
	"database/sql"
	"path/filepath"
	"testing"
	"time"

	_ "github.com/mattn/go-sqlite3"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/store"
)

func TestMigrateV2CreatesCorrectionTables(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	for _, table := range []string{"sessions", "turns", "actions", "corrections", "source_cursors", "candidate_corrections"} {
		if !s.TableExists(table) {
			t.Fatalf("table %q not created", table)
		}
	}
}

func TestCandidateSourceKindDefaultsToPromptPattern(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	now := time.Now()
	c := model.Candidate{
		ID:          "cand_source_default",
		Fingerprint: "fp_source_default",
		Name:        "source-default",
		Description: "test",
		RuleText:    "test",
		State:       model.CandidatePending,
		EventCount:  2,
		FirstSeen:   now,
		LastSeen:    now,
		UpdatedAt:   now,
	}
	if err := s.UpsertCandidate(c, nil, nil); err != nil {
		t.Fatal(err)
	}
	got, err := s.GetCandidate(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.SourceKind != model.CandidateSourcePromptPattern {
		t.Fatalf("source kind = %q, want prompt_pattern", got.SourceKind)
	}
}

func TestMigrateV6CreatesReplayApplicationsForLegacyDatabase(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "legacy.db")
	db, err := sql.Open("sqlite3", path)
	if err != nil {
		t.Fatal(err)
	}
	_, err = db.Exec(`CREATE TABLE candidates (
		id TEXT PRIMARY KEY,
		fingerprint TEXT NOT NULL UNIQUE,
		name TEXT NOT NULL,
		description TEXT NOT NULL,
		rule_text TEXT NOT NULL,
		state TEXT NOT NULL,
		event_count INTEGER NOT NULL,
		project_count INTEGER NOT NULL,
		source_count INTEGER NOT NULL,
		first_seen TEXT NOT NULL,
		last_seen TEXT NOT NULL,
		confidence TEXT NOT NULL,
		version INTEGER NOT NULL,
		updated_at TEXT NOT NULL
	)`)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	_, err = db.Exec(`INSERT INTO candidates
		(id, fingerprint, name, description, rule_text, state, event_count, project_count, source_count, first_seen, last_seen, confidence, version, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		"cand_legacy123", "fp_legacy123", "legacy", "legacy", "legacy rule", string(model.CandidatePending), 2, 1, 1, now, now, "medium", 1, now)
	if err != nil {
		t.Fatal(err)
	}
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}

	s, err := store.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	if !s.TableExists("replay_applications") {
		t.Fatal("replay_applications table not created")
	}
	got, err := s.GetCandidate("cand_legacy123")
	if err != nil {
		t.Fatal(err)
	}
	if got.State != model.CandidatePending {
		t.Fatalf("state = %s, want pending", got.State)
	}
	if got.SourceKind != model.CandidateSourcePromptPattern {
		t.Fatalf("source kind = %s, want prompt_pattern", got.SourceKind)
	}
}

func TestRecordReplayApplicationPersistsAppliedOnce(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	now := time.Now()
	c := model.Candidate{
		ID:          "cand_applyonce",
		Fingerprint: "fp_applyonce",
		Name:        "apply-once",
		Description: "test",
		RuleText:    "test",
		State:       model.CandidateProposalReady,
		EventCount:  3,
		Confidence:  "high",
		FirstSeen:   now,
		LastSeen:    now,
		UpdatedAt:   now,
	}
	if err := s.UpsertCandidate(c, nil, nil); err != nil {
		t.Fatal(err)
	}
	appliedAt := now.Add(time.Minute)
	app, err := s.RecordReplayApplication(model.ReplayApplication{
		ID:            "rapp_applyonce",
		CandidateID:   c.ID,
		Agent:         "codex",
		Project:       "agbox",
		PromptHash:    "hash123",
		PromptExcerpt: "현재 프로젝트 분석해줘",
		AppliedAt:     appliedAt,
	})
	if err != nil {
		t.Fatal(err)
	}
	if app.ID != "rapp_applyonce" {
		t.Fatalf("application id = %s, want explicit id", app.ID)
	}
	got, err := s.GetCandidate(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.State != model.CandidateAppliedOnce {
		t.Fatalf("state = %s, want applied_once", got.State)
	}
	apps, err := s.ListReplayApplications(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if len(apps) != 1 {
		t.Fatalf("applications = %d, want 1", len(apps))
	}
	if apps[0].Agent != "codex" || apps[0].Project != "agbox" || apps[0].PromptHash != "hash123" {
		t.Fatalf("application metadata = %+v", apps[0])
	}
	if !apps[0].AppliedAt.Equal(appliedAt) {
		t.Fatalf("applied_at = %s, want %s", apps[0].AppliedAt, appliedAt)
	}
}

func TestUpdateCandidateMetaIfStateRequiresExpectedState(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	now := time.Now()
	c := model.Candidate{
		ID:          "cand_guarded",
		Fingerprint: "fp_guarded",
		Name:        "guarded",
		Description: "test",
		RuleText:    "test",
		State:       model.CandidateAppliedOnce,
		EventCount:  3,
		Confidence:  "high",
		FirstSeen:   now,
		LastSeen:    now,
		UpdatedAt:   now,
	}
	if err := s.UpsertCandidate(c, nil, nil); err != nil {
		t.Fatal(err)
	}

	updated, err := s.UpdateCandidateMetaIfState(c.ID, model.CandidateProposalReady, store.CandidateMetaUpdate{
		State: model.CandidateSaveSuggested,
	})
	if err != nil {
		t.Fatal(err)
	}
	if updated {
		t.Fatal("updated with stale expected state, want false")
	}
	got, err := s.GetCandidate(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.State != model.CandidateAppliedOnce {
		t.Fatalf("state after stale update = %s, want applied_once", got.State)
	}

	updated, err = s.UpdateCandidateMetaIfState(c.ID, model.CandidateAppliedOnce, store.CandidateMetaUpdate{
		State: model.CandidateSaveSuggested,
	})
	if err != nil {
		t.Fatal(err)
	}
	if !updated {
		t.Fatal("updated = false, want true")
	}
	got, err = s.GetCandidate(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.State != model.CandidateSaveSuggested {
		t.Fatalf("state after guarded update = %s, want save_suggested", got.State)
	}
}

func TestUpsertCandidatePreservesAppliedOnceAndSaveSuggested(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	now := time.Now()
	for _, st := range []model.CandidateState{model.CandidateAppliedOnce, model.CandidateSaveSuggested} {
		c := model.Candidate{
			ID:          "cand_" + string(st),
			Fingerprint: "fp_" + string(st),
			Name:        "workflow",
			Description: "test",
			RuleText:    "test",
			State:       st,
			EventCount:  3,
			Confidence:  "high",
			FirstSeen:   now,
			LastSeen:    now,
			UpdatedAt:   now,
			Version:     4,
		}
		if err := s.UpsertCandidate(c, nil, nil); err != nil {
			t.Fatal(err)
		}
		incoming := c
		incoming.State = model.CandidatePending
		incoming.EventCount = 8
		incoming.Version = 1
		if err := s.UpsertCandidate(incoming, nil, nil); err != nil {
			t.Fatal(err)
		}
		got, err := s.GetCandidate(c.ID)
		if err != nil {
			t.Fatal(err)
		}
		if got.State != st {
			t.Fatalf("state after upsert = %s, want %s", got.State, st)
		}
		if got.Version != 4 {
			t.Fatalf("version after upsert = %d, want 4", got.Version)
		}
	}
}
