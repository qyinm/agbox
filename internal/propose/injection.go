package propose

import (
	"fmt"
	"strings"

	"github.com/hippoom/agbox/internal/model"
)

func RenderInjection(agent string, card model.EvidenceCard) string {
	c := card.Candidate
	pattern := humanPattern(c)
	weeklyMin := estimateWeeklyMinutes(c.EventCount)
	excerpts := card.Excerpts
	if len(excerpts) == 0 && c.RuleText != "" {
		excerpts = []string{c.RuleText}
	}
	causal := ""
	if len(card.Occurrences) > 0 {
		causal = card.Occurrences[0].SummaryLine()
	}

	var b strings.Builder
	fmt.Fprintf(&b, "<!-- agbox:proposal %s -->\n", c.ID)
	fmt.Fprintf(&b, "<!-- agbox:candidate %s -->\n", c.ID)
	fmt.Fprintln(&b, "## agbox sidecar — skill promotion candidate")
	fmt.Fprintln(&b)
	fmt.Fprintf(&b, "**Pattern:** %s\n", pattern)
	fmt.Fprintf(&b, "**Seen:** %d times across %d sessions (~%d min/week if uncorrected)\n", c.EventCount, c.ProjectCount, weeklyMin)
	fmt.Fprintf(&b, "**Confidence:** %s\n", c.Confidence)
	fmt.Fprintln(&b)
	if len(excerpts) > 0 {
		fmt.Fprintln(&b, "**Evidence excerpts:**")
		for _, ex := range excerpts {
			fmt.Fprintf(&b, "- %q\n", ex)
		}
		fmt.Fprintln(&b)
	}
	if causal != "" {
		fmt.Fprintf(&b, "**Causal example:** %s\n", causal)
		fmt.Fprintln(&b)
	}
	fmt.Fprintln(&b, "---")
	fmt.Fprintln(&b)
	fmt.Fprintln(&b, "### Instructions for the agent (not shown verbatim to user)")
	fmt.Fprintln(&b)
	fmt.Fprintf(&b, "You are receiving this because agbox detected a repeated user correction.\n\n")
	fmt.Fprintf(&b, "1. If the user has not already answered about candidate `%s` this session, briefly propose creating a skill for this pattern. One short paragraph. End with a clear yes/no question.\n", c.ID)
	fmt.Fprintln(&b, "2. If user says **yes**: create a skill in the **invoking agent's** native format:")
	for _, line := range skillPathLines(agent) {
		fmt.Fprintf(&b, "   - %s\n", line)
	}
	fmt.Fprintln(&b, "3. Skill must follow best practices:")
	fmt.Fprintln(&b, "   - YAML frontmatter with `name`, rich `description` (include trigger words from the pattern), and `agbox_candidate_id: "+c.ID+"`")
	fmt.Fprintln(&b, "   - Also include `<!-- agbox:candidate "+c.ID+" -->` in the SKILL body")
	fmt.Fprintln(&b, "   - Actionable body: what to do, what to avoid, examples")
	fmt.Fprintln(&b, "   - Not a copy-paste of evidence excerpts — synthesize a clear rule")
	fmt.Fprintf(&b, "4. If user says **no**: acknowledge, run `agbox reject %s` via Bash, do not ask again for 7 days.\n", c.ID)
	fmt.Fprintf(&b, "5. If user says **later** / ignores: run `agbox snooze %s`, do not ask again for 24 hours.\n", c.ID)
	fmt.Fprintln(&b, "6. Do NOT propose if user is mid-urgent task and their prompt is unrelated — wait for a natural pause.")
	fmt.Fprintf(&b, "<!-- /agbox:proposal -->\n")
	return b.String()
}

func skillPathLines(agent string) []string {
	switch agent {
	case "claude":
		return []string{"Claude Code (`propose claude`): `.claude/skills/<name>/SKILL.md`"}
	case "codex":
		return []string{
			"Codex (`propose codex`): `.agents/skills/<name>/SKILL.md` (repo) or `~/.codex/skills/<name>/SKILL.md` (user)",
		}
	case "grok":
		return []string{
			"Grok Build (`propose grok`): `.grok/skills/<name>/SKILL.md` (repo, preferred) or `~/.grok/skills/<name>/SKILL.md` (user)",
			"Grok skill `description` must include trigger phrases for auto-invocation",
		}
	default:
		return []string{"Native skill directory for the invoking agent"}
	}
}

func humanPattern(c model.Candidate) string {
	name := strings.ReplaceAll(c.Name, "-", " ")
	if name != "" {
		return name
	}
	return c.Description
}

func estimateWeeklyMinutes(eventCount int) int {
	if eventCount <= 0 {
		return 0
	}
	return eventCount * 4 * 3 / 7
}