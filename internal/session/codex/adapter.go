package codex

import (
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/session/jsonl"
)

type Adapter struct{}

func New() session.Adapter {
	return &Adapter{}
}

func init() {
	session.Register(New())
}

func (a *Adapter) Agent() string {
	return "codex"
}

func (a *Adapter) DiscoverSources() ([]session.Source, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, nil
	}
	root := filepath.Join(home, ".codex")
	info, err := os.Stat(root)
	if err != nil || !info.IsDir() {
		return nil, nil
	}

	var sources []session.Source
	_ = filepath.Walk(root, func(path string, fi os.FileInfo, walkErr error) error {
		if walkErr != nil || fi.IsDir() {
			return nil
		}
		if !strings.HasSuffix(strings.ToLower(fi.Name()), ".jsonl") {
			return nil
		}
		sources = append(sources, session.Source{
			Agent:   "codex",
			Path:    path,
			Project: filepath.Base(filepath.Dir(path)),
		})
		return nil
	})
	return sources, nil
}

func (a *Adapter) ParseDelta(src session.Source, cur session.Cursor) (session.ParseResult, error) {
	f, err := os.Open(src.Path)
	if err != nil {
		return session.ParseResult{}, err
	}
	defer f.Close()

	data, err := io.ReadAll(f)
	if err != nil {
		return session.ParseResult{}, err
	}
	fileHash := jsonl.HashBytes(data)
	sessionID := jsonl.StableID("ses_", src.Agent, src.Path)
	now := time.Now()

	acc, newOffset, err := jsonl.ProcessDelta(data, cur.LastOffset, jsonl.AnthropicHandler{}, jsonl.Meta{
		SessionID: sessionID,
		Agent:     src.Agent,
		Project:   src.Project,
		Now:       now,
	})
	if err != nil {
		return session.ParseResult{}, err
	}

	return session.ParseResult{
		Session: model.Session{
			ID:         sessionID,
			Agent:      src.Agent,
			Project:    src.Project,
			SourcePath: src.Path,
			SourceHash: fileHash,
			StartedAt:  now,
			UpdatedAt:  now,
		},
		Turns:       acc.Turns,
		Actions:     acc.Actions,
		Corrections: acc.Corrections,
		NewOffset:   newOffset,
		NewHash:     fileHash,
	}, nil
}