package scan

import (
	"fmt"
	"path/filepath"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/capture"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
	"github.com/hippoom/agbox/internal/store"
)

func TestRunCreatesCandidateForRepeatedSignal(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	for i := 0; i < 2; i++ {
		if _, err := capture.Capture(s, "Use bun, not npm.", capture.Options{Project: "repo", Source: "manual"}); err != nil {
			t.Fatal(err)
		}
	}
	result, err := Run(s, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Candidates) != 1 {
		t.Fatalf("candidates = %d, want 1", len(result.Candidates))
	}
	c := result.Candidates[0]
	if c.EventCount != 2 {
		t.Fatalf("event count = %d, want 2", c.EventCount)
	}
	if c.Name != "package-manager-workflow" {
		t.Fatalf("name = %q", c.Name)
	}
	if c.SourceKind != model.CandidateSourcePromptPattern {
		t.Fatalf("source kind = %q, want prompt_pattern", c.SourceKind)
	}
}

func TestRunClustersSimilarPackageManagerCorrections(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	inputs := []string{
		"Use pnpm, not npm.",
		"Please use pnpm instead of npm.",
	}
	for _, input := range inputs {
		if _, err := capture.Capture(s, input, capture.Options{Project: "repo", Source: "manual"}); err != nil {
			t.Fatal(err)
		}
	}
	result, err := Run(s, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Candidates) != 1 {
		t.Fatalf("candidates = %d, want 1", len(result.Candidates))
	}
	c := result.Candidates[0]
	if c.EventCount != 2 {
		t.Fatalf("event count = %d, want 2", c.EventCount)
	}
	if c.Name != "package-manager-workflow" {
		t.Fatalf("name = %q", c.Name)
	}
	if c.Fingerprint == privacy.HashSignal(privacy.NormalizeSignal(inputs[0])) {
		t.Fatalf("candidate used exact text hash, want semantic cluster fingerprint")
	}
}

func TestRunClustersWorkflowTaxonomySignals(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	inputs := []string{
		"Route changes require OpenAPI updates.",
		"When changing API routes, update the schema.",
	}
	for _, input := range inputs {
		if _, err := capture.Capture(s, input, capture.Options{Project: "repo", Source: "manual"}); err != nil {
			t.Fatal(err)
		}
	}
	result, err := Run(s, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Candidates) != 1 {
		t.Fatalf("candidates = %d, want 1", len(result.Candidates))
	}
	if result.Candidates[0].Name != "api-route-openapi-workflow" {
		t.Fatalf("name = %q", result.Candidates[0].Name)
	}
}

func TestRunClustersCorrectionsWithSameAction(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	normalized := privacy.NormalizeSignal("Use bun, not npm.")
	actionCmd := "npm install"
	now := time.Now()

	sess := model.Session{
		ID:         "sess_1",
		Agent:      "claude",
		Project:    "repo",
		SourcePath: "/tmp/session.jsonl",
		SourceHash: "abc",
		StartedAt:  now,
		UpdatedAt:  now,
	}
	if err := s.UpsertSession(sess); err != nil {
		t.Fatal(err)
	}

	turnAgent := model.Turn{
		ID:        "turn_agent",
		SessionID: sess.ID,
		TurnIndex: 1,
		Role:      "agent",
		EventType: "tool",
		CreatedAt: now,
	}
	turnUser := model.Turn{
		ID:        "turn_user",
		SessionID: sess.ID,
		TurnIndex: 2,
		Role:      "user",
		EventType: "message",
		CreatedAt: now.Add(time.Minute),
	}
	if err := s.InsertTurns([]model.Turn{turnAgent, turnUser}); err != nil {
		t.Fatal(err)
	}

	action := model.Action{
		ID:       "act_1",
		TurnID:   turnAgent.ID,
		ToolName: "run_terminal_cmd",
		Command:  actionCmd,
		FilePath: "",
		Excerpt:  actionCmd,
	}
	if err := s.InsertActions([]model.Action{action}); err != nil {
		t.Fatal(err)
	}

	for i := 0; i < 3; i++ {
		correction := model.Correction{
			ID:         fmt.Sprintf("cor_%d", i),
			SessionID:  sess.ID,
			TurnID:     turnUser.ID,
			ActionID:   action.ID,
			Hash:       privacy.HashSignal(normalized),
			Normalized: normalized,
			Excerpt:    "Use bun, not npm.",
			Agent:      "claude",
			Project:    "repo",
			CreatedAt:  now.Add(time.Duration(i+1) * time.Minute),
		}
		if err := s.InsertCorrection(correction); err != nil {
			t.Fatal(err)
		}
	}

	result, err := Run(s, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Candidates) != 1 {
		t.Fatalf("candidates = %d, want 1", len(result.Candidates))
	}
	if result.Candidates[0].EventCount < 2 {
		t.Fatalf("event count = %d, want >= 2", result.Candidates[0].EventCount)
	}
	if result.Candidates[0].SourceKind != model.CandidateSourceCorrection {
		t.Fatalf("source kind = %q, want correction", result.Candidates[0].SourceKind)
	}
}

func TestRunCreatesPromptCandidateWhenCorrectionsExist(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	seedSingleCorrection(t, s)
	for i := 0; i < 2; i++ {
		if _, err := capture.Capture(s, "현재 프로젝트 분석해줘.", capture.Options{Project: "repo", Source: "codex", Agent: "codex"}); err != nil {
			t.Fatal(err)
		}
	}

	result, err := Run(s, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Candidates) != 1 {
		t.Fatalf("candidates = %d, want 1 prompt candidate", len(result.Candidates))
	}
	c := result.Candidates[0]
	if c.SourceKind != model.CandidateSourcePromptPattern {
		t.Fatalf("source kind = %q, want prompt_pattern", c.SourceKind)
	}
	if c.EventCount != 2 {
		t.Fatalf("event count = %d, want 2", c.EventCount)
	}
}

func TestRunReusesLegacyLinkedPromptCandidate(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	var eventIDs []string
	for i := 0; i < 2; i++ {
		e, err := capture.Capture(s, "Use bun, not npm.", capture.Options{Project: "repo", Source: "manual"})
		if err != nil {
			t.Fatal(err)
		}
		eventIDs = append(eventIDs, e.ID)
	}
	legacyFingerprint := privacy.HashSignal("semantic:package-manager:bun-over-npm")
	now := time.Now()
	legacy := model.Candidate{
		ID:          "cand_" + legacyFingerprint[:12],
		Fingerprint: legacyFingerprint,
		Name:        "package-manager-workflow",
		Description: "legacy candidate",
		RuleText:    "Use bun, not npm.",
		State:       model.CandidatePending,
		EventCount:  2,
		FirstSeen:   now,
		LastSeen:    now,
		UpdatedAt:   now,
	}
	if err := s.UpsertCandidate(legacy, eventIDs, nil); err != nil {
		t.Fatal(err)
	}

	result, err := Run(s, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Candidates) != 1 {
		t.Fatalf("scan candidates = %d, want 1", len(result.Candidates))
	}
	candidates, err := s.ListCandidates("")
	if err != nil {
		t.Fatal(err)
	}
	if len(candidates) != 1 {
		t.Fatalf("stored candidates = %d, want 1", len(candidates))
	}
	if candidates[0].ID != legacy.ID {
		t.Fatalf("candidate id = %q, want legacy id %q", candidates[0].ID, legacy.ID)
	}
}

func TestRunSkipsPromptNoise(t *testing.T) {
	cases := []string{
		"ok",
		"12345",
		"<environment_context><cwd>/tmp/repo</cwd></environment_context>",
		"## agbox skill proposal instructions\nReply yes no or later.",
		"Generate 0 to 3 hyperpersonalized suggestions for the user based on their recent prompts.",
	}
	for _, input := range cases {
		t.Run(input, func(t *testing.T) {
			s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
			if err != nil {
				t.Fatal(err)
			}
			defer s.Close()
			for i := 0; i < 2; i++ {
				if _, err := capture.Capture(s, input, capture.Options{Project: "repo", Source: "codex", Agent: "codex"}); err != nil {
					t.Fatal(err)
				}
			}
			result, err := Run(s, 2)
			if err != nil {
				t.Fatal(err)
			}
			if len(result.Candidates) != 0 {
				t.Fatalf("candidates = %d, want 0", len(result.Candidates))
			}
		})
	}
}

func TestRunSkipsStructuredPromptNoiseWithoutExcerpt(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	for i := 0; i < 2; i++ {
		if _, err := capture.Capture(s, "<environment_context><cwd>/tmp/repo</cwd></environment_context>", capture.Options{
			Project:   "repo",
			Source:    "codex",
			Agent:     "codex",
			NoExcerpt: true,
		}); err != nil {
			t.Fatal(err)
		}
	}
	result, err := Run(s, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Candidates) != 0 {
		t.Fatalf("candidates = %d, want 0", len(result.Candidates))
	}
}

func seedSingleCorrection(t *testing.T, s *store.Store) {
	t.Helper()
	now := time.Now()
	sess := model.Session{
		ID:         "sess_existing",
		Agent:      "claude",
		Project:    "repo",
		SourcePath: "/tmp/session.jsonl",
		SourceHash: "abc",
		StartedAt:  now,
		UpdatedAt:  now,
	}
	if err := s.UpsertSession(sess); err != nil {
		t.Fatal(err)
	}
	turnAgent := model.Turn{ID: "turn_existing_agent", SessionID: sess.ID, TurnIndex: 1, Role: "agent", EventType: "tool", CreatedAt: now}
	turnUser := model.Turn{ID: "turn_existing_user", SessionID: sess.ID, TurnIndex: 2, Role: "user", EventType: "message", CreatedAt: now.Add(time.Minute)}
	if err := s.InsertTurns([]model.Turn{turnAgent, turnUser}); err != nil {
		t.Fatal(err)
	}
	action := model.Action{ID: "act_existing", TurnID: turnAgent.ID, ToolName: "run_terminal_cmd", Command: "npm install", Excerpt: "npm install"}
	if err := s.InsertActions([]model.Action{action}); err != nil {
		t.Fatal(err)
	}
	normalized := privacy.NormalizeSignal("Use bun, not npm.")
	correction := model.Correction{
		ID:         "cor_existing",
		SessionID:  sess.ID,
		TurnID:     turnUser.ID,
		ActionID:   action.ID,
		Hash:       privacy.HashSignal(normalized),
		Normalized: normalized,
		Excerpt:    "Use bun, not npm.",
		Agent:      "claude",
		Project:    "repo",
		CreatedAt:  now.Add(2 * time.Minute),
	}
	if err := s.InsertCorrection(correction); err != nil {
		t.Fatal(err)
	}
}
