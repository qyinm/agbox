package session_test

import (
	"testing"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/session"
)

func TestPairActionAndCorrection(t *testing.T) {
	turns := []model.Turn{
		{ID: "turn1", TurnIndex: 1, Role: "agent", EventType: "tool"},
		{ID: "turn2", TurnIndex: 2, Role: "user", EventType: "message"},
	}
	actions := []model.Action{{ID: "act1", TurnID: "turn1", Command: "npm install", Excerpt: "npm install"}}
	pairs := session.PairCorrections(turns, actions, map[string]string{"turn2": "use bun, not npm"})
	if len(pairs) != 1 {
		t.Fatalf("pairs = %d, want 1", len(pairs))
	}
	if pairs[0].Action.ID != "act1" {
		t.Fatalf("action ID = %q, want act1", pairs[0].Action.ID)
	}
	if pairs[0].UserText != "use bun, not npm" {
		t.Fatalf("user text = %q, want %q", pairs[0].UserText, "use bun, not npm")
	}
}

func TestPairCorrectionsSkipsNonCorrective(t *testing.T) {
	turns := []model.Turn{
		{ID: "turn1", TurnIndex: 1, Role: "agent", EventType: "tool"},
		{ID: "turn2", TurnIndex: 2, Role: "user", EventType: "message"},
	}
	actions := []model.Action{{ID: "act1", TurnID: "turn1", Command: "npm install", Excerpt: "npm install"}}

	cases := []struct {
		name string
		text string
	}{
		{name: "too short", text: "ok thanks"},
		{name: "matches agent output", text: "npm install"},
		{name: "no signal after normalize", text: "!!!@@@"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			pairs := session.PairCorrections(turns, actions, map[string]string{"turn2": tc.text})
			if len(pairs) != 0 {
				t.Fatalf("pairs = %d, want 0 for %q", len(pairs), tc.text)
			}
		})
	}
}

func TestPairCorrectionsRequiresPriorAction(t *testing.T) {
	turns := []model.Turn{
		{ID: "turn1", TurnIndex: 1, Role: "user", EventType: "message"},
	}
	pairs := session.PairCorrections(turns, nil, map[string]string{"turn1": "use bun, not npm"})
	if len(pairs) != 0 {
		t.Fatalf("pairs = %d, want 0 without prior action", len(pairs))
	}
}