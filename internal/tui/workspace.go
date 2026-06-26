package tui

import (
	"fmt"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

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
	opts          WorkspaceOptions
	active        WorkspaceScreen
	width         int
	height        int
	navCursor     int
	help          bool
	statusMessage string
}

func NewWorkspaceModel(opts WorkspaceOptions) WorkspaceModel {
	if opts.InitialScreen == "" {
		opts.InitialScreen = WorkspaceOverview
	}
	m := WorkspaceModel{opts: opts, active: opts.InitialScreen, width: 100, help: true}
	m.navCursor = navIndex(opts.InitialScreen)
	return m
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
		case "?", "shift+/":
			m.help = !m.help
		case "tab", "right":
			m.moveNav(1)
		case "shift+tab", "left":
			m.moveNav(-1)
		case "1", "2", "3", "4", "5", "6":
			m.selectNav(msg.String())
		case "r":
			m.statusMessage = "refresh queued"
		}
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
	}
	return m, nil
}

func (m WorkspaceModel) View() tea.View {
	return tea.NewView(m.Render())
}

func (m WorkspaceModel) Render() string {
	var b strings.Builder
	nav := workspaceNavStyle.Width(navWidth(m.width)).Render(m.renderNav())
	detailWidth := m.width - navWidth(m.width) - 4
	if detailWidth < 32 {
		detailWidth = 32
	}
	detail := workspaceDetailStyle.Width(detailWidth).Render(m.renderDetail())
	fmt.Fprintln(&b, lipgloss.JoinHorizontal(lipgloss.Top, nav, detail))
	fmt.Fprintln(&b, m.renderStatusBar())
	return b.String()
}

func (m *WorkspaceModel) moveNav(delta int) {
	m.navCursor += delta
	if m.navCursor < 0 {
		m.navCursor = len(workspaceNavItems) - 1
	}
	if m.navCursor >= len(workspaceNavItems) {
		m.navCursor = 0
	}
	m.active = workspaceNavItems[m.navCursor].screen
	m.statusMessage = ""
}

func (m *WorkspaceModel) selectNav(key string) {
	idx := int(key[0] - '1')
	if idx < 0 || idx >= len(workspaceNavItems) {
		return
	}
	m.navCursor = idx
	m.active = workspaceNavItems[idx].screen
	m.statusMessage = ""
}

func (m WorkspaceModel) renderNav() string {
	var b strings.Builder
	fmt.Fprintln(&b, titleStyle.Render("agbox"))
	fmt.Fprintln(&b, mutedStyle.Render("workspace"))
	fmt.Fprintln(&b)
	for i, item := range workspaceNavItems {
		line := fmt.Sprintf("%d %s", i+1, item.label)
		if item.screen == m.active {
			fmt.Fprintln(&b, workspaceNavActiveStyle.Render(line))
			continue
		}
		fmt.Fprintln(&b, workspaceNavItemStyle.Render(line))
	}
	if m.help {
		fmt.Fprintln(&b)
		fmt.Fprintln(&b, helpStyle.Render("tab nav"))
		fmt.Fprintln(&b, helpStyle.Render("r refresh"))
		fmt.Fprintln(&b, helpStyle.Render("q quit"))
	}
	return b.String()
}

func (m WorkspaceModel) renderDetail() string {
	switch m.active {
	case WorkspaceOverview:
		return m.renderOverview()
	case WorkspaceStatus:
		return m.renderPlaceholder("Status", "Watcher, managed hooks, store, sync, and workflow health.")
	case WorkspaceSources:
		return m.renderPlaceholder("Sources", "Discovered local agent session sources.")
	case WorkspaceWorkflows:
		return m.renderPlaceholder("Workflows", "Recorded Workflow inbox and replay-plan decisions.")
	case WorkspaceRepair:
		return m.renderPlaceholder("Repair", "Doctor checks and repair guidance.")
	case WorkspaceHelp:
		return m.renderHelpPlaceholder()
	default:
		return m.renderPlaceholder("Workspace", "Select a screen from the left navigation.")
	}
}

func (m WorkspaceModel) renderOverview() string {
	var b strings.Builder
	fmt.Fprintln(&b, sectionTitleStyle.Render("Overview"))
	fmt.Fprintln(&b, detailTitleStyle.Render("Current workspace"))
	fmt.Fprintln(&b)
	fmt.Fprintln(&b, labelStyle.Render("Setup health"))
	fmt.Fprintln(&b, bodyStyle.Render("  Watcher and managed hooks are summarized in the status bar."))
	fmt.Fprintln(&b)
	fmt.Fprintln(&b, labelStyle.Render("Workflow queue"))
	stats, err := m.storeStats()
	if err != nil {
		fmt.Fprintln(&b, bodyStyle.Render("  unavailable: "+err.Error()))
	} else {
		fmt.Fprintf(&b, "  %s  %s  %s\n",
			kv("recorded workflows", fmt.Sprintf("%d", stats.Candidates)),
			kv("events", fmt.Sprintf("%d", stats.Events)),
			kv("exports", fmt.Sprintf("%d", stats.Exports)),
		)
	}
	fmt.Fprintln(&b)
	fmt.Fprintln(&b, labelStyle.Render("Source freshness"))
	fmt.Fprintln(&b, bodyStyle.Render("  "+m.lastSyncLine()))
	fmt.Fprintln(&b)
	fmt.Fprintln(&b, labelStyle.Render("Next action"))
	fmt.Fprintln(&b, bodyStyle.Render("  Open Workflows to review replay plans, or press r to refresh local session data."))
	return b.String()
}

func (m WorkspaceModel) renderPlaceholder(title, body string) string {
	var b strings.Builder
	fmt.Fprintln(&b, sectionTitleStyle.Render(title))
	fmt.Fprintln(&b, bodyStyle.Render(body))
	return b.String()
}

func (m WorkspaceModel) renderHelpPlaceholder() string {
	var b strings.Builder
	fmt.Fprintln(&b, sectionTitleStyle.Render("Help"))
	if m.opts.HelpCommand != "" {
		fmt.Fprintf(&b, "%s\n", detailTitleStyle.Render("agbox "+m.opts.HelpCommand))
		if text, ok := m.opts.CommandHelp[m.opts.HelpCommand]; ok {
			fmt.Fprintln(&b, bodyStyle.Render(firstLine(text)))
			return b.String()
		}
	}
	fmt.Fprintln(&b, bodyStyle.Render("Browse agbox commands and workspace shortcuts."))
	return b.String()
}

func (m WorkspaceModel) renderStatusBar() string {
	message := m.statusMessage
	if message == "" {
		message = "ready"
	}
	line := fmt.Sprintf("watcher: pending | hooks: pending | sources: pending | sync: %s | r refresh | q quit", message)
	if m.width > 0 {
		line = truncate(line, m.width)
	}
	return workspaceStatusStyle.Render(line)
}

func (m WorkspaceModel) storeStats() (storeStats, error) {
	if m.opts.Store == nil {
		return storeStats{}, nil
	}
	stats, err := m.opts.Store.Stats()
	if err != nil {
		return storeStats{}, err
	}
	return storeStats{Events: stats.Events, Candidates: stats.Candidates, Exports: stats.Exports}, nil
}

func (m WorkspaceModel) lastSyncLine() string {
	if m.opts.Store == nil {
		return "last sync unavailable in this screen"
	}
	t, err := m.opts.Store.LatestCursorSync()
	if err != nil {
		return "last sync unavailable: " + err.Error()
	}
	if t.IsZero() {
		return "last sync: never"
	}
	return "last sync: " + formatWorkspaceTime(t)
}

type storeStats struct {
	Events     int
	Candidates int
	Exports    int
}

type workspaceNavItem struct {
	screen WorkspaceScreen
	label  string
}

var workspaceNavItems = []workspaceNavItem{
	{WorkspaceOverview, "Overview"},
	{WorkspaceStatus, "Status"},
	{WorkspaceSources, "Sources"},
	{WorkspaceWorkflows, "Workflows"},
	{WorkspaceRepair, "Repair"},
	{WorkspaceHelp, "Help"},
}

func navIndex(screen WorkspaceScreen) int {
	for i, item := range workspaceNavItems {
		if item.screen == screen {
			return i
		}
	}
	return 0
}

func navWidth(total int) int {
	switch {
	case total <= 60:
		return 16
	case total >= 120:
		return 24
	default:
		return 20
	}
}

func firstLine(s string) string {
	s = strings.TrimSpace(s)
	if idx := strings.IndexByte(s, '\n'); idx >= 0 {
		return s[:idx]
	}
	return s
}

func formatWorkspaceTime(t time.Time) string {
	age := time.Since(t)
	switch {
	case age < time.Minute:
		return "just now"
	case age < time.Hour:
		return fmt.Sprintf("%dm ago", int(age.Minutes()))
	case age < 24*time.Hour:
		return fmt.Sprintf("%dh ago", int(age.Hours()))
	default:
		return t.Format(time.RFC3339)
	}
}

var (
	workspaceNavStyle = lipgloss.NewStyle().
				BorderStyle(lipgloss.NormalBorder()).
				BorderRight(true).
				BorderForeground(lipgloss.Color("#334155")).
				PaddingRight(1)
	workspaceNavItemStyle = lipgloss.NewStyle().
				Foreground(lipgloss.Color("#CBD5E1")).
				PaddingLeft(1)
	workspaceNavActiveStyle = lipgloss.NewStyle().
				Bold(true).
				Foreground(lipgloss.Color("#F8FAFC")).
				Background(lipgloss.Color("#172033")).
				PaddingLeft(1)
	workspaceDetailStyle = lipgloss.NewStyle().
				PaddingLeft(2)
	workspaceStatusStyle = lipgloss.NewStyle().
				Foreground(lipgloss.Color("#D1D5DB")).
				Background(lipgloss.Color("#111827")).
				Padding(0, 1)
)
