package cursor

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/session"
)

type Adapter struct{}

func New() session.Adapter {
	return &Adapter{}
}

func init() {
	session.Register(New())
}

func (a *Adapter) Agent() string {
	return "cursor"
}

func (a *Adapter) DiscoverSources() ([]session.Source, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, nil
	}
	root := filepath.Join(home, "Library", "Application Support", "Cursor")
	info, err := os.Stat(root)
	if err != nil || !info.IsDir() {
		return nil, nil
	}

	var sources []session.Source
	_ = filepath.Walk(root, func(path string, fi os.FileInfo, walkErr error) error {
		if walkErr != nil {
			return nil
		}
		if fi.IsDir() {
			return nil
		}
		lower := strings.ToLower(fi.Name())
		if !strings.HasSuffix(lower, ".jsonl") && !strings.HasSuffix(lower, ".json") {
			return nil
		}
		project := filepath.Base(filepath.Dir(path))
		sources = append(sources, session.Source{
			Agent:   "cursor",
			Path:    path,
			Project: project,
		})
		return nil
	})
	return sources, nil
}

func (a *Adapter) ParseDelta(src session.Source, _ session.Cursor) (session.ParseResult, error) {
	// Cursor session format is unstable; discover sources but defer parsing.
	now := time.Now()
	sessionID := stableID("ses_", src.Agent, src.Path)
	return session.ParseResult{
		Session: model.Session{
			ID:         sessionID,
			Agent:      src.Agent,
			Project:    src.Project,
			SourcePath: src.Path,
			StartedAt:  now,
			UpdatedAt:  now,
		},
	}, nil
}

func stableID(prefix string, parts ...string) string {
	sum := sha256.Sum256([]byte(strings.Join(parts, "|")))
	return prefix + hex.EncodeToString(sum[:])[:16]
}