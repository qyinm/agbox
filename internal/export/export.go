package export

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/compile"
	"github.com/hippoom/agbox/internal/fsx"
	"github.com/hippoom/agbox/internal/manifest"
	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/store"
)

type Plan struct {
	ExportID    string `json:"export_id"`
	CandidateID string `json:"candidate_id"`
	Target      string `json:"target"`
	Path        string `json:"path"`
	Action      string `json:"action"`
	BytesBefore int    `json:"bytes_before"`
	BytesAfter  int    `json:"bytes_after"`
}

type Options struct {
	Target string
	Path   string
	DryRun bool
}

func BuildPlan(root string, c model.Candidate, opts Options) (Plan, []byte, error) {
	target := defaultTarget(opts.Target)
	artifact, err := compile.Render(c, target)
	if err != nil {
		return Plan{}, nil, err
	}
	target = artifact.Target
	rel := opts.Path
	if rel == "" {
		rel = defaultPath(target, c.Name)
	}
	full, err := fsx.ResolveInside(root, rel)
	if err != nil {
		return Plan{}, nil, err
	}
	before, err := fsx.ReadFile(full)
	if err != nil {
		return Plan{}, nil, err
	}
	after := mergeContent(before, artifact, c.ID)
	action := "append"
	if len(before) == 0 {
		action = "create"
	}
	return Plan{
		ExportID:    exportID(c.ID),
		CandidateID: c.ID,
		Target:      target,
		Path:        filepath.ToSlash(rel),
		Action:      action,
		BytesBefore: len(before),
		BytesAfter:  len(after),
	}, after, nil
}

func Apply(s *store.Store, root string, c model.Candidate, opts Options) (model.ExportRecord, error) {
	plan, after, err := BuildPlan(root, c, opts)
	if err != nil {
		return model.ExportRecord{}, err
	}
	rel := opts.Path
	if rel == "" {
		rel = defaultPath(plan.Target, c.Name)
	}
	full, err := fsx.ResolveInside(root, rel)
	if err != nil {
		return model.ExportRecord{}, err
	}
	before, err := fsx.ReadFile(full)
	if err != nil {
		return model.ExportRecord{}, err
	}
	backupPath, err := writeBackup(s.Path(), plan.ExportID, before)
	if err != nil {
		return model.ExportRecord{}, err
	}
	planJSON, _ := json.Marshal(plan)
	rec := model.ExportRecord{
		ID:          plan.ExportID,
		CandidateID: c.ID,
		Target:      plan.Target,
		Path:        filepath.ToSlash(rel),
		Status:      model.ExportApplied,
		PlanJSON:    string(planJSON),
		BackupPath:  backupPath,
		BeforeHash:  fsx.HashBytes(before),
		AfterHash:   fsx.HashBytes(after),
		AppliedAt:   time.Now(),
		CreatedAt:   time.Now(),
	}
	if err := fsx.AtomicWrite(full, after, 0o644); err != nil {
		rec.Status = model.ExportFailed
		_ = s.CreateExport(rec)
		return rec, err
	}
	if err := s.CreateExport(rec); err != nil {
		return model.ExportRecord{}, err
	}
	_ = s.SetCandidateState(c.ID, model.CandidateExported, "")
	candidates, _ := s.ListCandidates("")
	exports, _ := s.ListExports()
	_, _ = manifest.Write(root, candidates, exports)
	return rec, nil
}

func Rollback(s *store.Store, root string, exportID string) (model.ExportRecord, error) {
	rec, err := s.GetExport(exportID)
	if err != nil {
		return model.ExportRecord{}, err
	}
	if rec.Status != model.ExportApplied {
		return model.ExportRecord{}, fmt.Errorf("export %s is %s; only applied exports can be rolled back", exportID, rec.Status)
	}
	full, err := fsx.ResolveInside(root, rec.Path)
	if err != nil {
		return model.ExportRecord{}, err
	}
	backup, err := os.ReadFile(rec.BackupPath)
	if err != nil {
		return model.ExportRecord{}, err
	}
	if len(backup) == 0 {
		if err := os.Remove(full); err != nil && !os.IsNotExist(err) {
			return model.ExportRecord{}, err
		}
	} else if err := fsx.AtomicWrite(full, backup, 0o644); err != nil {
		return model.ExportRecord{}, err
	}
	rec.Status = model.ExportRolledBack
	rec.RolledBackAt = time.Now()
	if err := s.UpdateExport(rec); err != nil {
		return model.ExportRecord{}, err
	}
	_ = s.SetCandidateState(rec.CandidateID, model.CandidateApproved, "")
	candidates, _ := s.ListCandidates("")
	exports, _ := s.ListExports()
	_, _ = manifest.Write(root, candidates, exports)
	return rec, nil
}

func Repair(s *store.Store, root string) error {
	candidates, err := s.ListCandidates("")
	if err != nil {
		return err
	}
	exports, err := s.ListExports()
	if err != nil {
		return err
	}
	_, err = manifest.Write(root, candidates, exports)
	return err
}

func defaultTarget(target string) string {
	target = strings.TrimSpace(strings.ToLower(target))
	if target == "" {
		return "agents-md"
	}
	return target
}

func defaultPath(target, name string) string {
	switch target {
	case "claude":
		return "CLAUDE.md"
	case "cursor":
		return filepath.Join(".cursor", "rules", name+".mdc")
	case "cline":
		return filepath.Join(".clinerules", name+".md")
	default:
		return "AGENTS.md"
	}
}

func mergeContent(before []byte, artifact compile.Artifact, candidateID string) []byte {
	start := fmt.Sprintf("<!-- agbox:start %s -->", candidateID)
	end := fmt.Sprintf("<!-- agbox:end %s -->", candidateID)
	block := fmt.Sprintf("%s\n%s\n%s\n", start, strings.TrimSpace(artifact.Body), end)
	text := string(before)
	if strings.Contains(text, start) && strings.Contains(text, end) {
		pre := text[:strings.Index(text, start)]
		post := text[strings.Index(text, end)+len(end):]
		return []byte(strings.TrimRight(pre, "\n") + "\n\n" + block + strings.TrimLeft(post, "\n"))
	}
	if strings.TrimSpace(text) == "" {
		return []byte(block)
	}
	return []byte(strings.TrimRight(text, "\n") + "\n\n" + block)
}

func writeBackup(dbPath, exportID string, data []byte) (string, error) {
	dir := filepath.Join(filepath.Dir(dbPath), "exports", "backups")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", err
	}
	path := filepath.Join(dir, exportID+".bak")
	if err := os.WriteFile(path, data, 0o600); err != nil {
		return "", err
	}
	return path, nil
}

func exportID(candidateID string) string {
	return fmt.Sprintf("exp_%s_%d", strings.TrimPrefix(candidateID, "cand_"), time.Now().UnixNano())
}
