package manifest

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/hippoom/agbox/internal/fsx"
	"github.com/hippoom/agbox/internal/model"
)

type SkillPack struct {
	SchemaVersion string           `json:"schema_version"`
	GeneratedAt   string           `json:"generated_at"`
	Skills        []ManifestSkill  `json:"skills"`
	Exports       []ManifestExport `json:"exports"`
}

type ManifestSkill struct {
	ID          string `json:"id"`
	Name        string `json:"name"`
	State       string `json:"state"`
	Fingerprint string `json:"fingerprint"`
	Version     int    `json:"version"`
	Confidence  string `json:"confidence"`
}

type ManifestExport struct {
	ID          string `json:"id"`
	CandidateID string `json:"candidate_id"`
	Target      string `json:"target"`
	Path        string `json:"path"`
	Status      string `json:"status"`
	AfterHash   string `json:"after_hash"`
	CreatedAt   string `json:"created_at"`
}

func Write(root string, candidates []model.Candidate, exports []model.ExportRecord) (string, error) {
	pack := SkillPack{SchemaVersion: "1", GeneratedAt: time.Now().UTC().Format(time.RFC3339)}
	for _, c := range candidates {
		if c.State != model.CandidateApproved && c.State != model.CandidateExported {
			continue
		}
		pack.Skills = append(pack.Skills, ManifestSkill{
			ID: c.ID, Name: c.Name, State: string(c.State), Fingerprint: c.Fingerprint,
			Version: c.Version, Confidence: c.Confidence,
		})
	}
	for _, e := range exports {
		pack.Exports = append(pack.Exports, ManifestExport{
			ID: e.ID, CandidateID: e.CandidateID, Target: e.Target, Path: e.Path,
			Status: string(e.Status), AfterHash: e.AfterHash, CreatedAt: e.CreatedAt.UTC().Format(time.RFC3339),
		})
	}
	data, err := json.MarshalIndent(pack, "", "  ")
	if err != nil {
		return "", err
	}
	path, err := fsx.ResolveInside(root, ".agbox/skill-pack.json")
	if err != nil {
		return "", err
	}
	if err := fsx.AtomicWrite(path, append(data, '\n'), 0o644); err != nil {
		return "", err
	}
	return path, nil
}

func Verify(root string) error {
	path, err := fsx.ResolveInside(root, ".agbox/skill-pack.json")
	if err != nil {
		return err
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	var pack SkillPack
	if err := json.Unmarshal(data, &pack); err != nil {
		return err
	}
	if pack.SchemaVersion != "1" {
		return fmt.Errorf("unsupported manifest schema_version %q", pack.SchemaVersion)
	}
	for _, e := range pack.Exports {
		if e.Status != string(model.ExportApplied) {
			continue
		}
		full, err := fsx.ResolveInside(root, e.Path)
		if err != nil {
			return err
		}
		fileData, err := os.ReadFile(full)
		if err != nil {
			return err
		}
		if got := fsx.HashBytes(fileData); got != e.AfterHash {
			return fmt.Errorf("%s hash mismatch: manifest=%s current=%s", filepath.Clean(e.Path), e.AfterHash, got)
		}
	}
	return nil
}
