package tui

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/hippoom/agbox/internal/evidence"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/scan"
	"github.com/hippoom/agbox/internal/store"
)

type ReviewOptions struct {
	State      string
	MinRepeats int
	Limit      int
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

type pendingAction string

const (
	pendingNone    pendingAction = ""
	pendingApprove pendingAction = "approve"
	pendingReject  pendingAction = "reject"
)

type ReviewModel struct {
	service ReviewService
	data    ReviewData
	cursor  int
	pending pendingAction
	help    bool
	err     error
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
	m.data = data
	m.pending = pendingNone
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
	fmt.Fprintln(&b, mutedStyle.Render(fmt.Sprintf("scanned %d events  found %d repeated signals  showing %d %s candidates",
		m.data.Scanned, m.data.Found, len(m.data.Candidates), displayState(m.service.opts.State))))
	fmt.Fprintln(&b)
	if len(m.data.Candidates) == 0 {
		fmt.Fprintln(&b, sectionTitleStyle.Render("No candidates"))
		fmt.Fprintln(&b, bodyStyle.Render("No workflow candidates to review."))
		fmt.Fprintln(&b, mutedStyle.Render("Run agbox discover after a few agent sessions, or capture repeated prompts with agbox capture and agbox scan."))
		return b.String()
	}
	fmt.Fprintln(&b, sectionTitleStyle.Render("Candidates"))
	for i, c := range m.data.Candidates {
		line := fmt.Sprintf("%-30s %s %s %s",
			truncate(c.Name, 30),
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
		card := m.data.Cards[c.ID]
		fmt.Fprintln(&b, sectionTitleStyle.Render("Evidence"))
		fmt.Fprintln(&b, detailTitleStyle.Render(c.Name))
		fmt.Fprintf(&b, "%s  %s  %s  %s\n",
			kv("id", c.ID),
			kv("state", string(c.State)),
			kv("confidence", c.Confidence),
			kv("repeats", fmt.Sprintf("%d", c.EventCount)),
		)
		fmt.Fprintf(&b, "%s  %s\n", kv("projects", join(card.Projects)), kv("sources", join(card.Sources)))
		fmt.Fprintf(&b, "\n%s\n%s\n", labelStyle.Render("Reason"), bodyStyle.Render(card.Reason))
		fmt.Fprintf(&b, "\n%s\n%s\n", labelStyle.Render("Rule"), bodyStyle.Render(c.RuleText))
		if len(card.Excerpts) > 0 {
			fmt.Fprintln(&b)
			fmt.Fprintln(&b, labelStyle.Render("Excerpts"))
			for _, ex := range card.Excerpts {
				fmt.Fprintln(&b, excerptStyle.Render("  "+ex))
			}
		}
		fmt.Fprintln(&b)
		fmt.Fprintln(&b, hintStyle.Render("Next: agbox export "+c.ID+" --dry-run"))
	}
	if m.pending != pendingNone {
		fmt.Fprintf(&b, "\n%s\n", confirmStyle.Render(fmt.Sprintf("Confirm %s?  y confirm  n/esc cancel", m.pending)))
	} else if m.help {
		fmt.Fprintln(&b, "\n"+helpStyle.Render("j/down next  k/up previous  r refresh  a approve  x reject  ? help  q quit"))
	} else {
		fmt.Fprintln(&b, "\n"+helpStyle.Render("? help  q quit"))
	}
	return b.String()
}

func (m ReviewModel) handleKey(key string) ReviewModel {
	switch key {
	case "?", "shift+/":
		m.help = !m.help
		return m
	case "n", "esc":
		m.pending = pendingNone
		return m
	case "j", "down":
		if m.pending == pendingNone && m.cursor < len(m.data.Candidates)-1 {
			m.cursor++
		}
		return m
	case "k", "up":
		if m.pending == pendingNone && m.cursor > 0 {
			m.cursor--
		}
		return m
	case "r":
		if m.pending == pendingNone {
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

func (m ReviewModel) confirm() ReviewModel {
	c, ok := m.selected()
	if !ok {
		m.pending = pendingNone
		return m
	}
	switch m.pending {
	case pendingApprove:
		m.err = m.service.Approve(c.ID, c.Name)
	case pendingReject:
		m.err = m.service.Reject(c.ID)
	default:
		return m
	}
	if m.err != nil {
		return m
	}
	return m.Refresh()
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

func KeyForTest(m ReviewModel, key string) ReviewModel {
	return m.handleKey(key)
}
