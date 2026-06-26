package tui

import (
	"path/filepath"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

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

func TestWorkspaceModelNavigationChangesActiveScreen(t *testing.T) {
	m := NewWorkspaceModel(WorkspaceOptions{InitialScreen: WorkspaceOverview})
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
	if !strings.Contains(got, "Recorded Workflow inbox") {
		t.Fatalf("workflows detail missing:\n%s", got)
	}
}

func TestWorkspaceModelRefreshStatusBar(t *testing.T) {
	m := NewWorkspaceModel(WorkspaceOptions{InitialScreen: WorkspaceOverview})
	updated, _ := m.Update(tea.KeyPressMsg(tea.Key{Text: "r", Code: 'r'}))
	m = updated.(WorkspaceModel)
	got := stripANSI(m.Render())
	if !strings.Contains(got, "sync: refresh queued") {
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
