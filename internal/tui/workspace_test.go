package tui

import (
	"errors"
	"path/filepath"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/hippoom/agbox/internal/pipeline"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/store"
)

func TestWorkspaceModelRefreshRunsSyncAndSettlesStatus(t *testing.T) {
	s := openTestStore(t)
	defer s.Close()
	withWorkspaceSync(t, func(got *store.Store) (pipeline.BestEffortSyncResult, error) {
		if got != s {
			t.Fatal("refresh used unexpected store")
		}
		return pipeline.BestEffortSyncResult{Ingested: 2}, nil
	})

	m := NewWorkspaceModel(WorkspaceOptions{InitialScreen: WorkspaceOverview, Store: s})
	updated, cmd := m.Update(tea.KeyPressMsg(tea.Key{Text: "r", Code: 'r'}))
	m = updated.(WorkspaceModel)
	if cmd == nil {
		t.Fatal("refresh did not return command")
	}
	if !m.refreshing {
		t.Fatal("refreshing flag not set")
	}
	if got := stripANSI(m.Render()); !strings.Contains(got, "sync: syncing") {
		t.Fatalf("syncing status missing:\n%s", got)
	}
	updated, _ = m.Update(cmd())
	m = updated.(WorkspaceModel)
	if m.refreshing {
		t.Fatal("refreshing flag not cleared")
	}
	if got := stripANSI(m.Render()); !strings.Contains(got, "sync: synced 2 corrections") {
		t.Fatalf("synced status missing:\n%s", got)
	}
}

func TestWorkspaceModelRefreshShowsPartialAndFailure(t *testing.T) {
	s := openTestStore(t)
	defer s.Close()
	withWorkspaceSync(t, func(*store.Store) (pipeline.BestEffortSyncResult, error) {
		return pipeline.BestEffortSyncResult{Warning: errors.New("one source failed")}, nil
	})
	m := NewWorkspaceModel(WorkspaceOptions{InitialScreen: WorkspaceOverview, Store: s})
	updated, cmd := m.Update(tea.KeyPressMsg(tea.Key{Text: "r", Code: 'r'}))
	m = updated.(WorkspaceModel)
	updated, _ = m.Update(cmd())
	m = updated.(WorkspaceModel)
	if got := stripANSI(m.Render()); !strings.Contains(got, "sync: partial sync: one source failed") {
		t.Fatalf("partial sync status missing:\n%s", got)
	}

	withWorkspaceSync(t, func(*store.Store) (pipeline.BestEffortSyncResult, error) {
		return pipeline.BestEffortSyncResult{}, errors.New("database locked")
	})
	m = NewWorkspaceModel(WorkspaceOptions{InitialScreen: WorkspaceOverview, Store: s})
	updated, cmd = m.Update(tea.KeyPressMsg(tea.Key{Text: "r", Code: 'r'}))
	m = updated.(WorkspaceModel)
	updated, _ = m.Update(cmd())
	m = updated.(WorkspaceModel)
	got := stripANSI(m.Render())
	if !strings.Contains(got, "sync: refresh failed: database locked") || !strings.Contains(got, "Overview") {
		t.Fatalf("refresh failure status missing or screen lost:\n%s", got)
	}
}

func TestWorkspaceModelRefreshDoesNotOverlap(t *testing.T) {
	s := openTestStore(t)
	defer s.Close()
	m := NewWorkspaceModel(WorkspaceOptions{InitialScreen: WorkspaceOverview, Store: s})
	updated, cmd := m.Update(tea.KeyPressMsg(tea.Key{Text: "r", Code: 'r'}))
	if cmd == nil {
		t.Fatal("first refresh did not return command")
	}
	m = updated.(WorkspaceModel)
	_, second := m.Update(tea.KeyPressMsg(tea.Key{Text: "r", Code: 'r'}))
	if second != nil {
		t.Fatal("second refresh returned command while first was still running")
	}
}

func withWorkspaceSync(t *testing.T, fn func(*store.Store) (pipeline.BestEffortSyncResult, error)) {
	t.Helper()
	old := workspaceSyncBestEffort
	workspaceSyncBestEffort = fn
	t.Cleanup(func() { workspaceSyncBestEffort = old })
}

func TestWorkspaceModelRendersOverviewShell(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	s := openTestStore(t)
	defer s.Close()

	m := NewWorkspaceModel(WorkspaceOptions{
		InitialScreen: WorkspaceOverview,
		Store:         s,
		Project:       "repo",
	})
	got := stripANSI(m.Render())
	for _, want := range []string{
		"agbox",
		"Overview",
		"Status",
		"Sources",
		"Workflows",
		"Repair",
		"Setup health",
		"Workflow queue",
		"recorded workflows: 0",
		"last sync: never",
		"r refresh",
		"q quit",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("workspace render missing %q:\n%s", want, got)
		}
	}
}

func TestWorkspaceModelRendersStatusScreen(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	s := openTestStore(t)
	defer s.Close()

	m := NewWorkspaceModel(WorkspaceOptions{
		InitialScreen: WorkspaceStatus,
		Store:         s,
	})
	got := stripANSI(m.Render())
	for _, want := range []string{
		"Status",
		"watcher:",
		"managed hooks:",
		"store:",
		"last sync: never",
		"recorded workflows: 0",
		"events: 0",
		"exports: 0",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("status screen missing %q:\n%s", want, got)
		}
	}
}

func TestWorkspaceModelRendersSourcesScreen(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("XDG_CONFIG_HOME", filepath.Join(home, ".config"))
	m := NewWorkspaceModel(WorkspaceOptions{InitialScreen: WorkspaceSources})
	got := stripANSI(m.Render())
	for _, want := range []string{
		"Sources",
		"Local session paths",
		"No session sources discovered.",
		"sources: 0 discovered",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("sources screen missing %q:\n%s", want, got)
		}
	}
}

func TestWorkspaceModelRendersRepairScreen(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	s := openTestStore(t)
	defer s.Close()

	m := NewWorkspaceModel(WorkspaceOptions{
		InitialScreen: WorkspaceRepair,
		Store:         s,
	})
	got := stripANSI(m.Render())
	for _, want := range []string{
		"Repair",
		"store: OK",
		"events: 0",
		"recorded workflows: 0",
		"watcher:",
		"next: agbox init",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("repair screen missing %q:\n%s", want, got)
		}
	}
}

func TestWorkspaceModelRendersWorkflowCards(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	s := storeWithCandidates(t, "현재 프로젝트 분석해줘.", "현재 프로젝트 분석해줘.")
	defer s.Close()

	m := NewWorkspaceModel(WorkspaceOptions{
		InitialScreen: WorkspaceWorkflows,
		Store:         s,
	})
	got := stripANSI(m.Render())
	for _, want := range []string{
		"Workflows",
		"Recorded Workflow",
		"Replay Plan",
		"Evidence",
		"Safety",
		"does not re-run prior commands",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("workflow screen missing %q:\n%s", want, got)
		}
	}
}

func TestWorkspaceModelRendersEmptyWorkflowState(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	s := openTestStore(t)
	defer s.Close()

	m := NewWorkspaceModel(WorkspaceOptions{
		InitialScreen: WorkspaceWorkflows,
		Store:         s,
	})
	got := stripANSI(m.Render())
	for _, want := range []string{
		"No Recorded Workflows yet.",
		"Keep using your agents",
		"repeated prompts and corrections",
		"agbox demo",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("empty workflow screen missing %q:\n%s", want, got)
		}
	}
}

func TestWorkspaceModelRendersEvidenceDetail(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	s := storeWithCandidates(t, "Use bun, not npm.", "Use bun, not npm.")
	defer s.Close()
	if _, err := scan.Run(s, 2); err != nil {
		t.Fatal(err)
	}
	candidates, err := s.ListCandidates("")
	if err != nil {
		t.Fatal(err)
	}
	if len(candidates) == 0 {
		t.Fatal("expected candidate")
	}

	m := NewWorkspaceModel(WorkspaceOptions{
		InitialScreen: WorkspaceEvidence,
		Store:         s,
		EvidenceID:    candidates[0].ID,
	})
	got := stripANSI(m.Render())
	for _, want := range []string{
		"Evidence",
		"Package Manager Preference",
		"privacy:",
		"Reason",
		"Safety",
		"does not re-run prior commands",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("evidence screen missing %q:\n%s", want, got)
		}
	}
}

func TestWorkspaceModelEmbedsReviewModel(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	s := storeWithCandidates(t, "Use bun, not npm.", "Use bun, not npm.")
	defer s.Close()

	m := NewWorkspaceModel(WorkspaceOptions{
		InitialScreen: WorkspaceReview,
		Store:         s,
	})
	got := stripANSI(m.Render())
	for _, want := range []string{
		"agbox review",
		"Package Manager Preference",
		"Replay Plan",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("review workspace missing %q:\n%s", want, got)
		}
	}
	if !strings.Contains(got, "4 Workflows") {
		t.Fatalf("review workspace did not keep Workflows nav context:\n%s", got)
	}
}

func TestWorkspaceModelRendersHelpCommandBrowser(t *testing.T) {
	m := NewWorkspaceModel(WorkspaceOptions{
		InitialScreen: WorkspaceHelp,
		CommandHelp: map[string]string{
			"status": "Usage:\n  agbox status\n\nShow watcher state.",
			"sync":   "Usage:\n  agbox sync\n\nForce ingest.",
			"inbox":  "Usage:\n  agbox inbox\n\nShow Recorded Workflow cards.",
		},
	})
	got := stripANSI(m.Render())
	for _, want := range []string{
		"Command Browser",
		"Workspace",
		"agbox status",
		"Workflow",
		"agbox inbox",
		"Automation",
		"agbox sync",
		"Shortcuts",
		"r refresh",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("help browser missing %q:\n%s", want, got)
		}
	}
}

func TestWorkspaceModelRendersCommandHelpDetail(t *testing.T) {
	m := NewWorkspaceModel(WorkspaceOptions{
		InitialScreen: WorkspaceHelp,
		HelpCommand:   "status",
		CommandHelp: map[string]string{
			"status": "Usage:\n  agbox status\n\nShow watcher state, store path, last sync, and correction/candidate counts.",
		},
	})
	got := stripANSI(m.Render())
	for _, want := range []string{
		"agbox status",
		"Show watcher state",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("command help detail missing %q:\n%s", want, got)
		}
	}
	if strings.Contains(got, "Command Browser") {
		t.Fatalf("command detail included browser list:\n%s", got)
	}
}

func TestWorkspaceModelNavigationChangesActiveScreen(t *testing.T) {
	s := openTestStore(t)
	defer s.Close()
	m := NewWorkspaceModel(WorkspaceOptions{InitialScreen: WorkspaceOverview, Store: s})
	updated, _ := m.Update(tea.KeyPressMsg(tea.Key{Code: tea.KeyTab}))
	m = updated.(WorkspaceModel)
	if m.active != WorkspaceStatus {
		t.Fatalf("active after tab = %s, want %s", m.active, WorkspaceStatus)
	}
	updated, _ = m.Update(tea.KeyPressMsg(tea.Key{Text: "4", Code: '4'}))
	m = updated.(WorkspaceModel)
	if m.active != WorkspaceWorkflows {
		t.Fatalf("active after 4 = %s, want %s", m.active, WorkspaceWorkflows)
	}
	got := stripANSI(m.Render())
	if !strings.Contains(got, "No Recorded Workflows") {
		t.Fatalf("workflows detail missing:\n%s", got)
	}
}

func TestWorkspaceModelRefreshStatusBar(t *testing.T) {
	m := NewWorkspaceModel(WorkspaceOptions{InitialScreen: WorkspaceOverview})
	updated, _ := m.Update(tea.KeyPressMsg(tea.Key{Text: "r", Code: 'r'}))
	m = updated.(WorkspaceModel)
	got := stripANSI(m.Render())
	if !strings.Contains(got, "sync: syncing") {
		t.Fatalf("refresh status missing:\n%s", got)
	}
}

func TestWorkspaceModelHandlesNarrowWidths(t *testing.T) {
	m := NewWorkspaceModel(WorkspaceOptions{InitialScreen: WorkspaceOverview})
	updated, _ := m.Update(tea.WindowSizeMsg{Width: 34, Height: 16})
	m = updated.(WorkspaceModel)
	got := stripANSI(m.Render())
	if !strings.Contains(got, "Overview") || !strings.Contains(got, "r refresh") {
		t.Fatalf("narrow render missing expected content:\n%s", got)
	}
}
