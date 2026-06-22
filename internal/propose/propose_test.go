package propose_test

import (
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/propose"
	"github.com/hippoom/agbox/internal/store"
)

func TestMeetsThreshold(t *testing.T) {
	cases := []struct {
		c    model.Candidate
		want bool
	}{
		{model.Candidate{EventCount: 5, Confidence: "low"}, true},
		{model.Candidate{EventCount: 3, Confidence: "medium"}, true},
		{model.Candidate{EventCount: 3, Confidence: "low"}, false},
		{model.Candidate{EventCount: 2, Name: "package-manager-workflow", Confidence: "low"}, true},
	}
	for _, tc := range cases {
		if got := propose.MeetsThreshold(tc.c); got != tc.want {
			t.Fatalf("MeetsThreshold(%+v) = %v, want %v", tc.c, got, tc.want)
		}
	}
}

func TestRenderInjectionIncludesCandidateID(t *testing.T) {
	card := model.EvidenceCard{
		Candidate: model.Candidate{
			ID:         "cand_abc123",
			Name:       "package-manager-workflow",
			EventCount: 7,
			Confidence: "high",
			RuleText:   "use bun, not npm",
		},
		Excerpts: []string{"use bun, not npm"},
	}
	out := propose.RenderInjection("grok", card)
	if !strings.Contains(out, "cand_abc123") {
		t.Fatalf("injection missing candidate id: %s", out)
	}
	if !strings.Contains(out, ".grok/skills/") {
		t.Fatalf("injection missing grok skill path: %s", out)
	}
}

func TestMatchesSkillPath(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	path := filepath.Join(home, ".grok", "skills", "use-bun", "SKILL.md")
	if !propose.MatchesSkillPath("grok", path) {
		t.Fatalf("expected grok user skill path to match")
	}
	repoPath := "/tmp/project/.grok/skills/use-bun/SKILL.md"
	if !propose.MatchesSkillPath("grok", repoPath) {
		t.Fatalf("expected grok repo skill path to match")
	}
}

func TestProposeMarksCandidateProposed(t *testing.T) {
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "agbox.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	now := time.Now()
	c := model.Candidate{
		ID:          "cand_test12345",
		Fingerprint: "fp_test12345",
		Name:        "package-manager-workflow",
		Description: "test",
		RuleText:    "use bun not npm",
		State:       model.CandidateProposalReady,
		EventCount:  5,
		Confidence:  "high",
		FirstSeen:   now,
		LastSeen:    now,
		UpdatedAt:   now,
	}
	if err := s.UpsertSession(model.Session{
		ID: "ses_1", Agent: "grok", Project: "agbox", SourcePath: "/tmp/s.jsonl",
		SourceHash: "h", StartedAt: now, UpdatedAt: now,
	}); err != nil {
		t.Fatal(err)
	}
	if err := s.InsertTurns([]model.Turn{
		{ID: "turn_a", SessionID: "ses_1", TurnIndex: 1, Role: "agent", EventType: "tool", CreatedAt: now},
		{ID: "turn_u", SessionID: "ses_1", TurnIndex: 2, Role: "user", EventType: "message", CreatedAt: now},
	}); err != nil {
		t.Fatal(err)
	}
	if err := s.InsertActions([]model.Action{
		{ID: "act_1", TurnID: "turn_a", ToolName: "Shell", Command: "npm install", Excerpt: "npm install"},
	}); err != nil {
		t.Fatal(err)
	}
	if err := s.InsertCorrection(model.Correction{
		ID: "cor_1", SessionID: "ses_1", TurnID: "turn_u", ActionID: "act_1",
		Hash: "h1", Normalized: "use bun not npm", Excerpt: "use bun not npm",
		Agent: "grok", Project: "agbox", CreatedAt: now,
	}); err != nil {
		t.Fatal(err)
	}
	if err := s.UpsertCandidate(c, nil, []string{"cor_1"}); err != nil {
		t.Fatal(err)
	}

	payload, err := propose.Propose(s, "grok", "agbox")
	if err != nil {
		t.Fatal(err)
	}
	if payload == "" {
		t.Fatal("expected injection payload")
	}
	got, err := s.GetCandidate(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.State != model.CandidateProposed {
		t.Fatalf("state = %s, want proposed", got.State)
	}
}