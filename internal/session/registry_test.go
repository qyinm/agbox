package session_test

import (
	"testing"

	_ "github.com/hippoom/agbox/internal/session/claude"
	"github.com/hippoom/agbox/internal/session"
)

func TestRegistryListsAgents(t *testing.T) {
	agents := session.AgentNames()
	if len(agents) < 1 {
		t.Fatal("expected at least one registered adapter")
	}
}