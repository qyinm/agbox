package tui

import (
	"fmt"
	"os"
	"sort"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/hippoom/agbox/internal/connect"
	"github.com/hippoom/agbox/internal/doctor"
	"github.com/hippoom/agbox/internal/evidence"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/pipeline"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/watcher"
	"github.com/hippoom/agbox/internal/workflow"
)

var workspaceSyncBestEffort = pipeline.SyncBestEffort

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
	opts           WorkspaceOptions
	active         WorkspaceScreen
	width          int
	height         int
	navCursor      int
	workflowCursor int
	help           bool
	refreshing     bool
	statusMessage  string
	snapshot       workspaceSnapshot
	review         ReviewModel
}

func NewWorkspaceModel(opts WorkspaceOptions) WorkspaceModel {
	if opts.InitialScreen == "" {
		opts.InitialScreen = WorkspaceOverview
	}
	m := WorkspaceModel{opts: opts, active: opts.InitialScreen, width: 100, help: true}
	m.navCursor = navIndex(opts.InitialScreen)
	m.refreshSnapshot()
	if opts.Store != nil {
		m.review = NewReviewModel(NewReviewService(opts.Store, opts.ReviewOptions)).Refresh()
	}
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
			if m.refreshing {
				return m, nil
			}
			m.refreshing = true
			m.statusMessage = "syncing"
			return m, m.refreshCmd()
		default:
			m = m.handleActiveKey(msg.String())
		}
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
	case workspaceRefreshMsg:
		m.refreshing = false
		m.statusMessage = refreshStatusMessage(msg)
		m.refreshSnapshot()
		m.refreshActiveScreen()
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
		if item.screen == m.navActiveScreen() {
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
		return m.renderStatus()
	case WorkspaceSources:
		return m.renderSources()
	case WorkspaceWorkflows:
		return m.renderWorkflows()
	case WorkspaceReview:
		return m.renderReview()
	case WorkspaceEvidence:
		return m.renderEvidence()
	case WorkspaceRepair:
		return m.renderRepair()
	case WorkspaceHelp:
		return m.renderHelp()
	default:
		return m.renderPlaceholder("Workspace", "Select a screen from the left navigation.")
	}
}

func (m WorkspaceModel) handleActiveKey(key string) WorkspaceModel {
	switch m.active {
	case WorkspaceWorkflows:
		switch key {
		case "j", "down":
			if m.workflowCursor < len(m.snapshot.workflowCandidates)-1 {
				m.workflowCursor++
			}
		case "k", "up":
			if m.workflowCursor > 0 {
				m.workflowCursor--
			}
		}
	case WorkspaceReview:
		m.review = m.review.HandleKey(key)
	}
	return m
}

func (m *WorkspaceModel) refreshActiveScreen() {
	if m.active == WorkspaceReview && m.opts.Store != nil {
		m.review = m.review.Refresh()
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
	if m.snapshot.statsErr != nil {
		fmt.Fprintln(&b, bodyStyle.Render("  unavailable: "+m.snapshot.statsErr.Error()))
	} else if !m.snapshot.storeAvailable {
		fmt.Fprintln(&b, bodyStyle.Render("  store unavailable"))
	} else {
		fmt.Fprintf(&b, "  %s  %s  %s\n",
			kv("recorded workflows", fmt.Sprintf("%d", m.snapshot.stats.Candidates)),
			kv("events", fmt.Sprintf("%d", m.snapshot.stats.Events)),
			kv("exports", fmt.Sprintf("%d", m.snapshot.stats.Exports)),
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

func (m WorkspaceModel) renderStatus() string {
	var b strings.Builder
	fmt.Fprintln(&b, sectionTitleStyle.Render("Status"))
	fmt.Fprintln(&b, detailTitleStyle.Render("Health summary"))
	fmt.Fprintf(&b, "%s\n", kv("watcher", m.snapshot.watcher))
	fmt.Fprintf(&b, "%s\n", kv("managed hooks", m.snapshot.hooks))
	if !m.snapshot.storeAvailable {
		fmt.Fprintln(&b, bodyStyle.Render("store unavailable"))
		return b.String()
	}
	if m.snapshot.statsErr != nil {
		fmt.Fprintln(&b, bodyStyle.Render("store unavailable: "+m.snapshot.statsErr.Error()))
		return b.String()
	}
	if m.snapshot.correctionsErr != nil {
		fmt.Fprintln(&b, bodyStyle.Render("corrections unavailable: "+m.snapshot.correctionsErr.Error()))
		return b.String()
	}
	fmt.Fprintf(&b, "%s\n", kv("store", m.snapshot.storePath))
	fmt.Fprintf(&b, "%s\n", kv("last sync", m.lastSyncValue()))
	fmt.Fprintf(&b, "%s\n", kv("corrections", fmt.Sprintf("%d", m.snapshot.corrections)))
	fmt.Fprintf(&b, "%s\n", kv("recorded workflows", fmt.Sprintf("%d", m.snapshot.stats.Candidates)))
	fmt.Fprintf(&b, "%s\n", kv("events", fmt.Sprintf("%d", m.snapshot.stats.Events)))
	fmt.Fprintf(&b, "%s\n", kv("exports", fmt.Sprintf("%d", m.snapshot.stats.Exports)))
	return b.String()
}

func (m WorkspaceModel) renderSources() string {
	var b strings.Builder
	fmt.Fprintln(&b, sectionTitleStyle.Render("Sources"))
	fmt.Fprintln(&b, detailTitleStyle.Render("Local session paths"))
	entries := m.snapshot.sources
	if len(entries) == 0 {
		fmt.Fprintln(&b, bodyStyle.Render("No session sources discovered."))
		return b.String()
	}
	for _, entry := range entries {
		if entry.err != "" {
			fmt.Fprintf(&b, "%s\n", kv(entry.agent, entry.err))
			continue
		}
		fmt.Fprintf(&b, "%-8s %s\n", entry.agent, entry.path)
	}
	return b.String()
}

func (m WorkspaceModel) renderRepair() string {
	var b strings.Builder
	fmt.Fprintln(&b, sectionTitleStyle.Render("Repair"))
	if !m.snapshot.storeAvailable {
		fmt.Fprintln(&b, bodyStyle.Render("store unavailable"))
		return b.String()
	}
	report := m.snapshot.repairReport
	if report.OK {
		fmt.Fprintln(&b, hintStyle.Render("All checks passed."))
	} else {
		fmt.Fprintln(&b, confirmStyle.Render("Attention needed"))
	}
	for _, line := range report.Lines {
		fmt.Fprintln(&b, bodyStyle.Render(line))
	}
	return b.String()
}

func (m WorkspaceModel) renderWorkflows() string {
	var b strings.Builder
	fmt.Fprintln(&b, sectionTitleStyle.Render("Workflows"))
	if m.snapshot.workflowErr != nil {
		fmt.Fprintln(&b, bodyStyle.Render("error: "+m.snapshot.workflowErr.Error()))
		return b.String()
	}
	candidates := m.snapshot.workflowCandidates
	cards := m.snapshot.workflowCards
	if len(candidates) == 0 {
		fmt.Fprintln(&b, detailTitleStyle.Render("No Recorded Workflows"))
		fmt.Fprintln(&b, bodyStyle.Render("No Recorded Workflows yet."))
		fmt.Fprintln(&b, mutedStyle.Render("Keep using your agents; agbox records repeated prompts and corrections automatically."))
		fmt.Fprintln(&b, mutedStyle.Render("Try agbox demo, or run doctor if setup looks wrong."))
		return b.String()
	}
	fmt.Fprintln(&b, detailTitleStyle.Render("Recorded Workflows"))
	fmt.Fprintf(&b, "%s\n", mutedStyle.Render(fmt.Sprintf("showing %d %s recorded workflows", len(candidates), displayState(m.opts.WorkflowState))))
	cursor := m.workflowCursor
	if cursor >= len(candidates) {
		cursor = len(candidates) - 1
	}
	for i, c := range candidates {
		card := workflow.Build(cards[c.ID])
		line := fmt.Sprintf("%-30s %s %s", truncate(card.Name, 30), card.Lifecycle, metric("repeats", fmt.Sprintf("%d", c.EventCount)))
		if i == cursor {
			fmt.Fprintln(&b, selectedRowStyle.Render("  "+line))
			continue
		}
		fmt.Fprintln(&b, rowStyle.Render("  "+line))
	}
	selected := candidates[cursor]
	evidenceCard := cards[selected.ID]
	card := workflow.Build(evidenceCard)
	fmt.Fprintln(&b)
	fmt.Fprintln(&b, detailTitleStyle.Render(card.Name))
	fmt.Fprintf(&b, "%s  %s  %s\n", kv("id", selected.ID), kv("lifecycle", card.Lifecycle), kv("confidence", selected.Confidence))
	fmt.Fprintf(&b, "\n%s\n%s\n", labelStyle.Render("When It Applies"), bodyStyle.Render(card.WhenItApplies))
	fmt.Fprintf(&b, "\n%s\n", labelStyle.Render("Replay Plan"))
	for i, step := range card.ReplayPlan {
		fmt.Fprintf(&b, "%s\n", bodyStyle.Render(fmt.Sprintf("  %d. %s", i+1, step)))
	}
	fmt.Fprintf(&b, "\n%s\n%s\n", labelStyle.Render("Evidence"), bodyStyle.Render(card.EvidenceSummary))
	fmt.Fprintf(&b, "\n%s\n%s\n", labelStyle.Render("Safety"), bodyStyle.Render(card.SafetyNote))
	fmt.Fprintln(&b, helpStyle.Render("j/k navigate  r refresh  q quit"))
	return b.String()
}

func (m WorkspaceModel) renderReview() string {
	if m.opts.Store == nil {
		return m.renderPlaceholder("Review", "store unavailable")
	}
	return m.review.Render()
}

func (m WorkspaceModel) renderEvidence() string {
	var b strings.Builder
	fmt.Fprintln(&b, sectionTitleStyle.Render("Evidence"))
	if m.opts.Store == nil {
		fmt.Fprintln(&b, bodyStyle.Render("store unavailable"))
		return b.String()
	}
	if strings.TrimSpace(m.opts.EvidenceID) == "" {
		fmt.Fprintln(&b, bodyStyle.Render("Select a Recorded Workflow from Workflows, or run agbox evidence <candidate-id>."))
		return b.String()
	}
	card, err := evidence.Build(m.opts.Store, m.opts.EvidenceID)
	if err != nil {
		fmt.Fprintln(&b, bodyStyle.Render("error: "+err.Error()))
		return b.String()
	}
	workflowCard := workflow.Build(card)
	c := card.Candidate
	fmt.Fprintln(&b, detailTitleStyle.Render(workflowCard.Name))
	fmt.Fprintf(&b, "%s  %s  %s\n", kv("id", c.ID), kv("state", string(c.State)), kv("privacy", card.Privacy))
	fmt.Fprintf(&b, "%s  %s  %s\n", kv("source", string(c.SourceKind)), kv("repeats", fmt.Sprintf("%d", c.EventCount)), kv("confidence", c.Confidence))
	fmt.Fprintf(&b, "\n%s\n%s\n", labelStyle.Render("Reason"), bodyStyle.Render(card.Reason))
	fmt.Fprintf(&b, "\n%s\n%s\n", labelStyle.Render("Safety"), bodyStyle.Render(workflowCard.SafetyNote))
	if len(card.Excerpts) > 0 {
		fmt.Fprintln(&b)
		fmt.Fprintln(&b, labelStyle.Render("Excerpts"))
		for _, excerpt := range card.Excerpts {
			fmt.Fprintln(&b, excerptStyle.Render("  "+excerpt))
		}
	}
	if len(card.Occurrences) > 0 {
		fmt.Fprintln(&b)
		fmt.Fprintln(&b, labelStyle.Render("Occurrences"))
		for i, occurrence := range card.Occurrences {
			if i >= 5 {
				break
			}
			fmt.Fprintln(&b, bodyStyle.Render(fmt.Sprintf("  %d. %s", i+1, occurrence.SummaryLine())))
		}
	}
	return b.String()
}

func (m *WorkspaceModel) refreshSnapshot() {
	snapshot := workspaceSnapshot{
		watcher:       workspaceWatcherState(),
		hooks:         workspaceHookSummary(),
		sources:       workspaceSources(),
		workflowCards: map[string]model.EvidenceCard{},
	}
	snapshot.sourceSummary = summarizeWorkspaceSources(snapshot.sources)
	if m.opts.Store == nil {
		m.snapshot = snapshot
		return
	}
	snapshot.storeAvailable = true
	stats, err := m.opts.Store.Stats()
	if err != nil {
		snapshot.statsErr = err
	} else {
		snapshot.storePath = stats.Path
		snapshot.stats = storeStats{Events: stats.Events, Candidates: stats.Candidates, Exports: stats.Exports}
	}
	snapshot.corrections, snapshot.correctionsErr = m.opts.Store.CountCorrections()
	snapshot.lastSync, snapshot.lastSyncErr = m.opts.Store.LatestCursorSync()
	snapshot.repairReport = doctor.Run(m.opts.Store)
	candidates, cards, err := m.workflowData()
	if err != nil {
		snapshot.workflowErr = err
	} else {
		snapshot.workflowCandidates = candidates
		snapshot.workflowCards = cards
	}
	m.snapshot = snapshot
}

func (m WorkspaceModel) workflowData() ([]model.Candidate, map[string]model.EvidenceCard, error) {
	if m.opts.Store == nil {
		return nil, nil, fmt.Errorf("store unavailable")
	}
	if _, err := scan.Run(m.opts.Store, 2); err != nil {
		return nil, nil, err
	}
	state := normalizeState(m.opts.WorkflowState)
	candidates, err := m.opts.Store.ListCandidates(state)
	if err != nil {
		return nil, nil, err
	}
	cards := make(map[string]model.EvidenceCard, len(candidates))
	for _, c := range candidates {
		card, err := evidence.Build(m.opts.Store, c.ID)
		if err != nil {
			return nil, nil, err
		}
		cards[c.ID] = card
	}
	return candidates, cards, nil
}

type workspaceSnapshot struct {
	watcher            string
	hooks              string
	sources            []workspaceSource
	sourceSummary      string
	storeAvailable     bool
	storePath          string
	stats              storeStats
	statsErr           error
	corrections        int
	correctionsErr     error
	lastSync           time.Time
	lastSyncErr        error
	repairReport       doctor.Report
	workflowCandidates []model.Candidate
	workflowCards      map[string]model.EvidenceCard
	workflowErr        error
}

func (m WorkspaceModel) renderHelp() string {
	var b strings.Builder
	fmt.Fprintln(&b, sectionTitleStyle.Render("Help"))
	if m.opts.HelpCommand != "" {
		fmt.Fprintf(&b, "%s\n", detailTitleStyle.Render("agbox "+m.opts.HelpCommand))
		if text, ok := m.opts.CommandHelp[m.opts.HelpCommand]; ok {
			fmt.Fprintln(&b, bodyStyle.Render(strings.TrimSpace(text)))
			return b.String()
		}
	}
	fmt.Fprintln(&b, detailTitleStyle.Render("Command Browser"))
	for _, group := range helpCommandGroups {
		fmt.Fprintln(&b)
		fmt.Fprintln(&b, labelStyle.Render(group.title))
		for _, command := range group.commands {
			fmt.Fprintf(&b, "  %-18s %s\n", "agbox "+command, commandSummary(command, m.opts.CommandHelp))
		}
	}
	fmt.Fprintln(&b)
	fmt.Fprintln(&b, labelStyle.Render("Shortcuts"))
	fmt.Fprintln(&b, bodyStyle.Render("  tab/shift+tab navigate  1-6 jump  r refresh  ? help  q quit"))
	return b.String()
}

func (m WorkspaceModel) renderStatusBar() string {
	message := m.statusMessage
	if message == "" {
		message = "ready"
	}
	line := fmt.Sprintf("sync: %s | watcher: %s | hooks: %s | sources: %s | r refresh | q quit",
		message, m.snapshot.watcher, m.snapshot.hooks, m.snapshot.sourceSummary)
	if m.width > 0 && m.width < 70 {
		line = fmt.Sprintf("sync: %s | r refresh | q quit", message)
	}
	if m.width >= 70 {
		line = truncate(line, m.width)
	}
	return workspaceStatusStyle.Render(line)
}

func (m WorkspaceModel) refreshCmd() tea.Cmd {
	s := m.opts.Store
	return func() tea.Msg {
		if s == nil {
			return workspaceRefreshMsg{err: fmt.Errorf("store unavailable")}
		}
		result, err := workspaceSyncBestEffort(s)
		return workspaceRefreshMsg{result: result, err: err}
	}
}

type workspaceRefreshMsg struct {
	result pipeline.BestEffortSyncResult
	err    error
}

func refreshStatusMessage(msg workspaceRefreshMsg) string {
	if msg.err != nil {
		return "refresh failed: " + msg.err.Error()
	}
	if msg.result.Warning != nil {
		return "partial sync: " + msg.result.Warning.Error()
	}
	if msg.result.IngestSkipped {
		return "synced recently"
	}
	if msg.result.Ingested == 1 {
		return "synced 1 correction"
	}
	return fmt.Sprintf("synced %d corrections", msg.result.Ingested)
}

func (m WorkspaceModel) lastSyncLine() string {
	if !m.snapshot.storeAvailable {
		return "last sync unavailable in this screen"
	}
	if m.snapshot.lastSyncErr != nil {
		return "last sync unavailable: " + m.snapshot.lastSyncErr.Error()
	}
	if m.snapshot.lastSync.IsZero() {
		return "last sync: never"
	}
	return "last sync: " + formatWorkspaceTime(m.snapshot.lastSync)
}

func (m WorkspaceModel) lastSyncValue() string {
	if !m.snapshot.storeAvailable {
		return "unavailable"
	}
	if m.snapshot.lastSyncErr != nil {
		return "FAIL " + m.snapshot.lastSyncErr.Error()
	}
	if m.snapshot.lastSync.IsZero() {
		return "never"
	}
	return formatWorkspaceTime(m.snapshot.lastSync)
}

type workspaceSource struct {
	agent string
	path  string
	err   string
}

func workspaceWatcherState() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return "unknown"
	}
	ws := watcher.Status(home)
	if ws.Running {
		if ws.PID > 0 {
			return fmt.Sprintf("running pid=%d", ws.PID)
		}
		return "running"
	}
	if ws.Installed {
		return "installed"
	}
	return "not installed"
}

func workspaceHookSummary() string {
	statuses := connect.StatusAll()
	connected := 0
	var needs []string
	for _, status := range statuses {
		if status.State == "connected" {
			connected++
			continue
		}
		if !status.OK {
			needs = append(needs, status.Agent)
		}
	}
	out := fmt.Sprintf("%d/%d connected", connected, len(statuses))
	if len(needs) > 0 {
		out += " (" + strings.Join(needs, ", ") + " need attention)"
	}
	return out
}

func workspaceSources() []workspaceSource {
	var entries []workspaceSource
	for _, adapter := range session.All() {
		sources, err := adapter.DiscoverSources()
		if err != nil {
			entries = append(entries, workspaceSource{agent: adapter.Agent(), err: err.Error()})
			continue
		}
		for _, source := range sources {
			entries = append(entries, workspaceSource{agent: source.Agent, path: source.Path})
		}
	}
	sort.Slice(entries, func(i, j int) bool {
		if entries[i].agent == entries[j].agent {
			return entries[i].path < entries[j].path
		}
		return entries[i].agent < entries[j].agent
	})
	return entries
}

func summarizeWorkspaceSources(entries []workspaceSource) string {
	failed := 0
	count := 0
	for _, entry := range entries {
		if entry.err != "" {
			failed++
			continue
		}
		count++
	}
	if failed > 0 {
		return fmt.Sprintf("%d discovered, %d failed", count, failed)
	}
	return fmt.Sprintf("%d discovered", count)
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
	if screen == WorkspaceReview || screen == WorkspaceEvidence {
		screen = WorkspaceWorkflows
	}
	for i, item := range workspaceNavItems {
		if item.screen == screen {
			return i
		}
	}
	return 0
}

func (m WorkspaceModel) navActiveScreen() WorkspaceScreen {
	if m.active == WorkspaceReview || m.active == WorkspaceEvidence {
		return WorkspaceWorkflows
	}
	return m.active
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

type helpCommandGroup struct {
	title    string
	commands []string
}

var helpCommandGroups = []helpCommandGroup{
	{"Workspace", []string{"status", "sources", "inbox", "review", "doctor", "repair", "help"}},
	{"Workflow", []string{"evidence", "apply", "accept", "reject", "snooze", "approve"}},
	{"Setup / demos", []string{"init", "connect", "disconnect", "beta", "demo"}},
	{"Automation", []string{"capture", "scan", "discover", "sync", "hook", "compile", "export", "manifest", "impact", "audit", "telemetry"}},
}

func commandSummary(command string, help map[string]string) string {
	text, ok := help[command]
	if !ok {
		if command == "help" {
			return "Open this command browser."
		}
		return ""
	}
	lines := strings.Split(strings.TrimSpace(text), "\n")
	for _, line := range lines {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "Usage:") || strings.HasPrefix(line, "agbox ") {
			continue
		}
		return line
	}
	return firstLine(text)
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
