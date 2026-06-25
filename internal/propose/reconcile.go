package propose

import (
	"io/fs"
	"os"
	"path/filepath"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/store"
)

type ReconcileAcceptedSkillsResult struct {
	Accepted int
	Paths    []string
}

func ReconcileAcceptedSkills(s *store.Store) (ReconcileAcceptedSkillsResult, error) {
	cwd, _ := os.Getwd()
	return ReconcileAcceptedSkillsInRoots(s, defaultSkillRoots(cwd))
}

func ReconcileAcceptedSkillsInRoots(s *store.Store, roots []string) (ReconcileAcceptedSkillsResult, error) {
	var result ReconcileAcceptedSkillsResult
	seen := map[string]bool{}
	for _, root := range roots {
		root = filepath.Clean(root)
		if root == "." || root == "" || seen[root] {
			continue
		}
		seen[root] = true
		info, err := os.Stat(root)
		if err != nil || !info.IsDir() {
			continue
		}
		err = filepath.WalkDir(root, func(path string, d fs.DirEntry, walkErr error) error {
			if walkErr != nil || d.IsDir() || filepath.Base(path) != "SKILL.md" {
				return nil
			}
			accepted, err := reconcileSkillFile(s, path)
			if err != nil {
				return err
			}
			if accepted {
				result.Accepted++
				result.Paths = append(result.Paths, path)
			}
			return nil
		})
		if err != nil {
			return result, err
		}
	}
	return result, nil
}

func defaultSkillRoots(cwd string) []string {
	var roots []string
	if cwd != "" {
		roots = append(roots,
			filepath.Join(cwd, ".agents", "skills"),
			filepath.Join(cwd, ".claude", "skills"),
			filepath.Join(cwd, ".grok", "skills"),
		)
	}
	if home, err := os.UserHomeDir(); err == nil && home != "" {
		roots = append(roots,
			filepath.Join(home, ".agents", "skills"),
			filepath.Join(home, ".codex", "skills"),
			filepath.Join(home, ".claude", "skills"),
			filepath.Join(home, ".grok", "skills"),
		)
	}
	return roots
}

func reconcileSkillFile(s *store.Store, path string) (bool, error) {
	content, err := os.ReadFile(path)
	if err != nil {
		return false, nil
	}
	candidateID := extractCandidateID(string(content))
	if candidateID == "" {
		return false, nil
	}
	c, err := s.GetCandidate(candidateID)
	if err != nil || !canReconcileToAccepted(c.State) {
		return false, nil
	}
	absPath, err := filepath.Abs(path)
	if err != nil {
		absPath = filepath.Clean(path)
	}
	return true, Accept(s, candidateID, absPath)
}

func canReconcileToAccepted(state model.CandidateState) bool {
	switch state {
	case model.CandidatePending,
		model.CandidateProposalReady,
		model.CandidateProposed,
		model.CandidateSnoozed,
		model.CandidateApproved:
		return true
	default:
		return false
	}
}
