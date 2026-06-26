package tui

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/hippoom/agbox/internal/store"
)

type WorkspaceScreen string

const (
	WorkspaceOverview  WorkspaceScreen = "overview"
	WorkspaceStatus    WorkspaceScreen = "status"
	WorkspaceSources   WorkspaceScreen = "sources"
	WorkspaceRepair    WorkspaceScreen = "repair"
	WorkspaceWorkflows WorkspaceScreen = "workflows"
	WorkspaceReview    WorkspaceScreen = "review"
	WorkspaceEvidence  WorkspaceScreen = "evidence"
	WorkspaceHelp      WorkspaceScreen = "help"
)

type WorkspaceOptions struct {
	InitialScreen WorkspaceScreen
	Store         *store.Store
	Project       string

	WorkflowState string
	EvidenceID    string
	HelpCommand   string
	CommandHelp   map[string]string
	ReviewOptions ReviewOptions
}

type WorkspaceModel struct {
	opts   WorkspaceOptions
	active WorkspaceScreen
}

func NewWorkspaceModel(opts WorkspaceOptions) WorkspaceModel {
	if opts.InitialScreen == "" {
		opts.InitialScreen = WorkspaceOverview
	}
	return WorkspaceModel{opts: opts, active: opts.InitialScreen}
}

func (m WorkspaceModel) Init() tea.Cmd {
	return nil
}

func (m WorkspaceModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		switch msg.String() {
		case "q", "ctrl+c":
			return m, tea.Quit
		}
	}
	return m, nil
}

func (m WorkspaceModel) View() tea.View {
	return tea.NewView(m.Render())
}

func (m WorkspaceModel) Render() string {
	var b strings.Builder
	fmt.Fprintln(&b, titleStyle.Render("agbox"))
	fmt.Fprintf(&b, "%s\n\n", mutedStyle.Render("workspace"))
	fmt.Fprintln(&b, sectionTitleStyle.Render(string(m.active)))
	switch m.active {
	case WorkspaceHelp:
		if m.opts.HelpCommand != "" {
			fmt.Fprintf(&b, "Help: agbox %s\n", m.opts.HelpCommand)
		} else {
			fmt.Fprintln(&b, "Help: commands")
		}
	default:
		fmt.Fprintf(&b, "Screen: %s\n", m.active)
	}
	fmt.Fprintln(&b)
	fmt.Fprintln(&b, helpStyle.Render("q quit"))
	return b.String()
}
