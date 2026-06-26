package tui

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/hippoom/agbox/internal/evidence"
	agexport "github.com/hippoom/agbox/internal/export"
	"github.com/hippoom/agbox/internal/fsx"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/workflow"
)

type ReviewOptions struct {
	State      string
	MinRepeats int
	Limit      int
	Project    string
}

type viewMode int

const (
	viewList viewMode = iota
	viewDrillDown
	viewExportTarget
)

var exportTargets = []struct {
	key    string
	target string
	label  string
}{
	{"1", "agents-md", "AGENTS.md"},
	{"2", "claude", "CLAUDE.md"},
	{"3", "cursor", "Cursor rules"},
	{"4", "cline", "Cline rules"},
}

type ReviewService struct {
	store *store.Store
	opts  ReviewOptions
}

type ReviewData struct {
	Scanned    int
	Found      int
	Candidates []model.Candidate
	Cards      map[string]model.EvidenceCard
}

func NewReviewService(s *store.Store, opts ReviewOptions) ReviewService {
	if opts.State == "" {
		opts.State = string(model.CandidatePending)
	}
	if opts.MinRepeats <= 0 {
		opts.MinRepeats = 2
	}
	return ReviewService{store: s, opts: opts}
}

func (s ReviewService) Load() (ReviewData, error) {
	result, err := scan.Run(s.store, s.opts.MinRepeats)
	if err != nil {
		return ReviewData{}, err
	}
	state := normalizeState(s.opts.State)
	candidates, err := s.store.ListCandidates(state)
	if err != nil {
		return ReviewData{}, err
	}
	if s.opts.Limit > 0 && len(candidates) > s.opts.Limit {
		candidates = candidates[:s.opts.Limit]
	}
	cards := make(map[string]model.EvidenceCard, len(candidates))
	for _, c := range candidates {
		card, err := evidence.Build(s.store, c.ID)
		if err != nil {
			return ReviewData{}, err
		}
		cards[c.ID] = card
	}
	return ReviewData{
		Scanned:    result.Scanned,
		Found:      len(result.Candidates),
		Candidates: candidates,
		Cards:      cards,
	}, nil
}

func (s ReviewService) Approve(id, name string) error {
	return s.store.SetCandidateState(id, model.CandidateApproved, name)
}

func (s ReviewService) Reject(id string) error {
	return s.store.SetCandidateState(id, model.CandidateRejected, "")
}

func (s ReviewService) Export(candidateID, target string) (string, error) {
	c, err := s.store.GetCandidate(candidateID)
	if err != nil {
		return "", err
	}
	if c.State != model.CandidateApproved && c.State != model.CandidateExported {
		return "", fmt.Errorf("candidate %s is not approved", candidateID)
	}
	root, err := fsx.ProjectRoot()
	if err != nil {
		return "", err
	}
	rec, err := agexport.Apply(s.store, root, c, agexport.Options{Target: target})
	if err != nil {
		return "", err
	}
	return fmt.Sprintf("exported %s path=%s target=%s; undo: agbox export rollback %s", rec.ID, rec.Path, rec.Target, rec.ID), nil
}

type pendingAction string

const (
	pendingNone    pendingAction = ""
	pendingApprove pendingAction = "approve"
	pendingReject  pendingAction = "reject"
)

type ReviewModel struct {
	service          ReviewService
	data             ReviewData
	cursor           int
	occurrenceCursor int
	pending          pendingAction
	view             viewMode
	showAllProjects  bool
	statusMessage    string
	help             bool
	err              error
}

func NewReviewModel(service ReviewService) ReviewModel {
	return ReviewModel{service: service, help: true}
}

func (m ReviewModel) Init() tea.Cmd {
	return nil
}

func (m ReviewModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		switch msg.String() {
		case "q", "ctrl+c":
			return m, tea.Quit
		}
		return m.handleKey(msg.String()), nil
	}
	return m, nil
}

func (m ReviewModel) View() tea.View {
	return tea.NewView(m.Render())
}

func (m ReviewModel) Refresh() ReviewModel {
	previousID := ""
	if selected, ok := m.selected(); ok {
		previousID = selected.ID
	}
	data, err := m.service.Load()
	m.err = err
	if err != nil {
		return m
	}
	data = m.applyProjectFilter(data)
	m.data = data
	m.pending = pendingNone
	m.view = viewList
	m.occurrenceCursor = 0
	m.cursor = nearestCursor(data.Candidates, previousID, m.cursor)
	return m
}

func (m ReviewModel) Render() string {
	if m.err != nil {
		var b strings.Builder
		fmt.Fprintln(&b, titleStyle.Render("agbox review"))
		fmt.Fprintf(&b, "error: %v\n", m.err)
		return b.String()
	}
	var b strings.Builder
	fmt.Fprintln(&b, titleStyle.Render("agbox review"))
	filterLabel := "all projects"
	if m.activeProjectFilter() != "" {
		filterLabel = "project " + m.activeProjectFilter()
	}
	fmt.Fprintln(&b, mutedStyle.Render(fmt.Sprintf("scanned %d events  found %d repeated signals  showing %d %s recorded workflows  filter=%s",
		m.data.Scanned, m.data.Found, len(m.data.Candidates), displayState(m.service.opts.State), filterLabel)))
	if m.statusMessage != "" {
		fmt.Fprintln(&b, hintStyle.Render(m.statusMessage))
	}
	fmt.Fprintln(&b)
	if m.view == viewDrillDown {
		return m.renderDrillDown()
	}
	if m.view == viewExportTarget {
		return m.renderExportTarget()
	}
	if len(m.data.Candidates) == 0 {
		fmt.Fprintln(&b, sectionTitleStyle.Render("No Recorded Workflows"))
		fmt.Fprintln(&b, bodyStyle.Render("No Recorded Workflows to review."))
		fmt.Fprintln(&b, mutedStyle.Render("Run agbox inbox after a few agent sessions, or use agbox demo to see the loop without touching your data."))
		return b.String()
	}
	fmt.Fprintln(&b, sectionTitleStyle.Render("Recorded Workflows"))
	for i, c := range m.data.Candidates {
		card := workflow.Build(m.data.Cards[c.ID])
		line := fmt.Sprintf("%-30s %s %s %s",
			truncate(card.Name, 30),
			stateBadge(c.State),
			metric("confidence", c.Confidence),
			metric("repeats", fmt.Sprintf("%d", c.EventCount)),
		)
		if i == m.cursor {
			fmt.Fprintln(&b, selectedRowStyle.Render("  "+line))
			continue
		}
		fmt.Fprintln(&b, rowStyle.Render("  "+line))
	}
	fmt.Fprintln(&b)
	if c, ok := m.selected(); ok {
		evidenceCard := m.data.Cards[c.ID]
		card := workflow.Build(evidenceCard)
		fmt.Fprintln(&b, sectionTitleStyle.Render("Workflow"))
		fmt.Fprintln(&b, detailTitleStyle.Render(card.Name))
		fmt.Fprintf(&b, "%s  %s  %s  %s\n",
			kv("id", c.ID),
			kv("lifecycle", card.Lifecycle),
			kv("source", string(c.SourceKind)),
			kv("confidence", c.Confidence),
		)
		fmt.Fprintf(&b, "%s\n", kv("repeats", fmt.Sprintf("%d", c.EventCount)))
		fmt.Fprintf(&b, "%s  %s\n", kv("projects", join(evidenceCard.Projects)), kv("sources", join(evidenceCard.Sources)))
		fmt.Fprintf(&b, "\n%s\n%s\n", labelStyle.Render("When It Applies"), bodyStyle.Render(card.WhenItApplies))
		fmt.Fprintf(&b, "\n%s\n", labelStyle.Render("Replay Plan"))
		for i, step := range card.ReplayPlan {
			fmt.Fprintf(&b, "%s\n", bodyStyle.Render(fmt.Sprintf("  %d. %s", i+1, step)))
		}
		fmt.Fprintf(&b, "\n%s\n%s\n", labelStyle.Render("Evidence"), bodyStyle.Render(card.EvidenceSummary))
		fmt.Fprintf(&b, "\n%s\n%s\n", labelStyle.Render("Safety"), bodyStyle.Render(card.SafetyNote))
		if len(evidenceCard.Occurrences) > 0 {
			fmt.Fprintln(&b)
			fmt.Fprintln(&b, labelStyle.Render("Occurrences"))
			limit := len(evidenceCard.Occurrences)
			if limit > 5 {
				limit = 5
			}
			for i := 0; i < limit; i++ {
				line := fmt.Sprintf("  %d. %s", i+1, evidenceCard.Occurrences[i].SummaryLine())
				if i == m.occurrenceCursor {
					fmt.Fprintln(&b, selectedRowStyle.Render(line))
					continue
				}
				fmt.Fprintln(&b, excerptStyle.Render(line))
			}
		} else if len(evidenceCard.Excerpts) > 0 {
			fmt.Fprintln(&b)
			fmt.Fprintln(&b, labelStyle.Render("Excerpts"))
			for _, ex := range evidenceCard.Excerpts {
				fmt.Fprintln(&b, excerptStyle.Render("  "+ex))
			}
		}
		if c.State == model.CandidateApproved || c.State == model.CandidateExported {
			fmt.Fprintln(&b, hintStyle.Render("Press e to export"))
		}
	}
	if m.pending != pendingNone {
		fmt.Fprintf(&b, "\n%s\n", confirmStyle.Render(fmt.Sprintf("Confirm %s?  y confirm  n/esc cancel", m.pending)))
	} else if m.help {
		fmt.Fprintln(&b, "\n"+helpStyle.Render("j/k navigate  enter drill-down  f filter  a approve  x reject  e export  r refresh  ? help  q quit"))
	} else {
		fmt.Fprintln(&b, "\n"+helpStyle.Render("? help  q quit"))
	}
	return b.String()
}

func (m ReviewModel) renderDrillDown() string {
	var b strings.Builder
	fmt.Fprintln(&b, titleStyle.Render("agbox review"))
	c, ok := m.selected()
	if !ok {
		return b.String()
	}
	card := m.data.Cards[c.ID]
	occ, ok := m.selectedOccurrence(card)
	if !ok {
		return b.String()
	}
	fmt.Fprintln(&b, sectionTitleStyle.Render("Occurrence"))
	fmt.Fprintln(&b, detailTitleStyle.Render(occ.SummaryLine()))
	for _, step := range occ.DrillDown {
		fmt.Fprintln(&b, bodyStyle.Render("  "+step.Format()))
	}
	fmt.Fprintln(&b, mutedStyle.Render("esc back"))
	return b.String()
}

func (m ReviewModel) renderExportTarget() string {
	var b strings.Builder
	fmt.Fprintln(&b, titleStyle.Render("agbox review"))
	c, ok := m.selected()
	if !ok {
		return b.String()
	}
	fmt.Fprintln(&b, sectionTitleStyle.Render("Export target"))
	fmt.Fprintln(&b, detailTitleStyle.Render(c.Name))
	for _, item := range exportTargets {
		fmt.Fprintf(&b, "  %s  %s (%s)\n", item.key, item.label, item.target)
	}
	fmt.Fprintln(&b, mutedStyle.Render("esc cancel"))
	return b.String()
}

func (m ReviewModel) handleKey(key string) ReviewModel {
	if m.view == viewDrillDown {
		if key == "esc" {
			m.view = viewList
		}
		return m
	}
	if m.view == viewExportTarget {
		if key == "esc" {
			m.view = viewList
			return m
		}
		for _, item := range exportTargets {
			if key == item.key {
				return m.runExport(item.target)
			}
		}
		return m
	}
	switch key {
	case "?", "shift+/":
		m.help = !m.help
		return m
	case "n", "esc":
		m.pending = pendingNone
		return m
	case "f":
		if m.pending == pendingNone {
			m.showAllProjects = !m.showAllProjects
			return m.Refresh()
		}
		return m
	case "enter":
		if m.pending != pendingNone {
			return m
		}
		if c, ok := m.selected(); ok {
			card := m.data.Cards[c.ID]
			if len(card.Occurrences) > 0 {
				m.view = viewDrillDown
			}
		}
		return m
	case "]", "}":
		if c, ok := m.selected(); ok {
			card := m.data.Cards[c.ID]
			if m.occurrenceCursor < len(card.Occurrences)-1 {
				m.occurrenceCursor++
			}
		}
		return m
	case "[", "{":
		if m.occurrenceCursor > 0 {
			m.occurrenceCursor--
		}
		return m
	case "e":
		if m.pending != pendingNone {
			return m
		}
		if c, ok := m.selected(); ok {
			if c.State == model.CandidateApproved || c.State == model.CandidateExported {
				m.view = viewExportTarget
				m.statusMessage = ""
			}
		}
		return m
	case "j", "down":
		if m.pending == pendingNone && m.cursor < len(m.data.Candidates)-1 {
			m.cursor++
			m.occurrenceCursor = 0
		}
		return m
	case "k", "up":
		if m.pending == pendingNone && m.cursor > 0 {
			m.cursor--
			m.occurrenceCursor = 0
		}
		return m
	case "r":
		if m.pending == pendingNone {
			m.statusMessage = ""
			return m.Refresh()
		}
		return m
	case "a":
		if _, ok := m.selected(); ok && m.pending == pendingNone {
			m.pending = pendingApprove
		}
		return m
	case "x":
		if _, ok := m.selected(); ok && m.pending == pendingNone {
			m.pending = pendingReject
		}
		return m
	case "y":
		return m.confirm()
	default:
		return m
	}
}

func (m ReviewModel) HandleKey(key string) ReviewModel {
	return m.handleKey(key)
}

func (m ReviewModel) runExport(target string) ReviewModel {
	c, ok := m.selected()
	if !ok {
		m.view = viewList
		return m
	}
	msg, err := m.service.Export(c.ID, target)
	m.view = viewList
	if err != nil {
		m.err = err
		return m
	}
	m.statusMessage = msg
	return m.Refresh()
}

func (m ReviewModel) confirm() ReviewModel {
	c, ok := m.selected()
	if !ok {
		m.pending = pendingNone
		return m
	}
	switch m.pending {
	case pendingApprove:
		m.err = m.service.Approve(c.ID, c.Name)
		if m.err == nil {
			m.data.Candidates[m.cursor].State = model.CandidateApproved
		}
	case pendingReject:
		m.err = m.service.Reject(c.ID)
		if m.err == nil {
			return m.Refresh()
		}
	default:
		return m
	}
	m.pending = pendingNone
	return m
}

func (m ReviewModel) selected() (model.Candidate, bool) {
	if m.cursor < 0 || m.cursor >= len(m.data.Candidates) {
		return model.Candidate{}, false
	}
	return m.data.Candidates[m.cursor], true
}

func nearestCursor(candidates []model.Candidate, previousID string, previous int) int {
	if len(candidates) == 0 {
		return 0
	}
	for i, c := range candidates {
		if c.ID == previousID {
			return i
		}
	}
	if previous >= len(candidates) {
		return len(candidates) - 1
	}
	if previous < 0 {
		return 0
	}
	return previous
}

func normalizeState(state string) string {
	state = strings.ToLower(strings.TrimSpace(state))
	if state == "all" {
		return ""
	}
	return state
}

func displayState(state string) string {
	state = normalizeState(state)
	if state == "" {
		return "all"
	}
	return state
}

func join(values []string) string {
	if len(values) == 0 {
		return "-"
	}
	return strings.Join(values, ", ")
}

func metric(label, value string) string {
	return mutedStyle.Render(label+"=") + value
}

func kv(label, value string) string {
	return labelStyle.Render(label+": ") + bodyStyle.Render(value)
}

func truncate(s string, width int) string {
	if width <= 1 {
		return s
	}
	runes := []rune(s)
	if len(runes) <= width {
		return s
	}
	return string(runes[:width-3]) + "..."
}

func stateBadge(state model.CandidateState) string {
	switch state {
	case model.CandidateProposalReady:
		return badgeBase.Copy().Foreground(lipgloss.Color("#DDD6FE")).Background(lipgloss.Color("#4C1D95")).Render(" proposal_ready ")
	case model.CandidateProposed:
		return badgeBase.Copy().Foreground(lipgloss.Color("#BAE6FD")).Background(lipgloss.Color("#075985")).Render(" proposed ")
	case model.CandidateAppliedOnce:
		return badgeBase.Copy().Foreground(lipgloss.Color("#FDE68A")).Background(lipgloss.Color("#713F12")).Render(" applied_once ")
	case model.CandidateSaveSuggested:
		return badgeBase.Copy().Foreground(lipgloss.Color("#FBCFE8")).Background(lipgloss.Color("#831843")).Render(" save_suggested ")
	case model.CandidateAccepted:
		return badgeBase.Copy().Foreground(lipgloss.Color("#A7F3D0")).Background(lipgloss.Color("#065F46")).Render(" accepted ")
	case model.CandidateSnoozed:
		return badgeBase.Copy().Foreground(lipgloss.Color("#FDE68A")).Background(lipgloss.Color("#713F12")).Render(" snoozed ")
	case model.CandidateApproved:
		return badgeBase.Copy().Foreground(lipgloss.Color("#34D399")).Background(lipgloss.Color("#064E3B")).Render(" approved ")
	case model.CandidateRejected:
		return badgeBase.Copy().Foreground(lipgloss.Color("#FCA5A5")).Background(lipgloss.Color("#7F1D1D")).Render(" rejected ")
	case model.CandidateExported:
		return badgeBase.Copy().Foreground(lipgloss.Color("#FBBF24")).Background(lipgloss.Color("#78350F")).Render(" exported ")
	default:
		return badgeBase.Copy().Foreground(lipgloss.Color("#93C5FD")).Background(lipgloss.Color("#1E3A8A")).Render(" pending ")
	}
}

var (
	titleStyle = lipgloss.NewStyle().
			Bold(true).
			Foreground(lipgloss.Color("#7DD3FC")).
			MarginBottom(0)
	mutedStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#94A3B8"))
	sectionTitleStyle = lipgloss.NewStyle().
				Bold(true).
				Foreground(lipgloss.Color("#E2E8F0")).
				BorderStyle(lipgloss.NormalBorder()).
				BorderBottom(true).
				BorderForeground(lipgloss.Color("#334155")).
				PaddingBottom(0)
	detailTitleStyle = lipgloss.NewStyle().
				Bold(true).
				Foreground(lipgloss.Color("#F8FAFC")).
				MarginTop(0)
	rowStyle = lipgloss.NewStyle().
			PaddingTop(0).
			PaddingBottom(0)
	selectedRowStyle = lipgloss.NewStyle().
				Foreground(lipgloss.Color("#F8FAFC")).
				Background(lipgloss.Color("#172033")).
				BorderStyle(lipgloss.ThickBorder()).
				BorderLeft(true).
				BorderForeground(lipgloss.Color("#38BDF8"))
	labelStyle = lipgloss.NewStyle().
			Bold(true).
			Foreground(lipgloss.Color("#CBD5E1"))
	bodyStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#E5E7EB"))
	excerptStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#CBD5E1")).
			BorderStyle(lipgloss.NormalBorder()).
			BorderLeft(true).
			BorderForeground(lipgloss.Color("#475569")).
			PaddingLeft(1)
	hintStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#34D399"))
	helpStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#94A3B8"))
	confirmStyle = lipgloss.NewStyle().
			Bold(true).
			Foreground(lipgloss.Color("#FDE68A")).
			Background(lipgloss.Color("#713F12")).
			Padding(0, 1)
	badgeBase = lipgloss.NewStyle().
			Bold(true)
)

func (m ReviewModel) applyProjectFilter(data ReviewData) ReviewData {
	project := m.activeProjectFilter()
	if project == "" {
		return data
	}
	filtered := make([]model.Candidate, 0, len(data.Candidates))
	for _, c := range data.Candidates {
		card := data.Cards[c.ID]
		if projectMatches(card.Projects, project) {
			filtered = append(filtered, c)
		}
	}
	data.Candidates = filtered
	return data
}

func (m ReviewModel) activeProjectFilter() string {
	if m.showAllProjects || strings.TrimSpace(m.service.opts.Project) == "" {
		return ""
	}
	return strings.TrimSpace(m.service.opts.Project)
}

func projectMatches(projects []string, project string) bool {
	for _, p := range projects {
		if p == project {
			return true
		}
	}
	return false
}

func (m ReviewModel) selectedOccurrence(card model.EvidenceCard) (model.Occurrence, bool) {
	if m.occurrenceCursor < 0 || m.occurrenceCursor >= len(card.Occurrences) {
		return model.Occurrence{}, false
	}
	return card.Occurrences[m.occurrenceCursor], true
}

func KeyForTest(m ReviewModel, key string) ReviewModel {
	return m.HandleKey(key)
}
