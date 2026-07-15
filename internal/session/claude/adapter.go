package claude

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

func New() *Adapter {
	return &Adapter{}
}

func init() {
	session.Register(New())
}

func (a *Adapter) Agent() string {
	return "claude"
}

func (a *Adapter) Runnable() bool { return true }

func (a *Adapter) RootSpecs() []session.RootSpec {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil
	}
	return []session.RootSpec{{
		Path: filepath.Join(home, ".claude", "projects"), Class: session.RootActive, Recursive: true,
		Match: func(rel string, _ os.DirEntry) bool { return strings.EqualFold(filepath.Ext(rel), ".jsonl") },
	}}
}

func (a *Adapter) DiscoverSources() ([]session.Source, error) {
	return session.DiscoverRoots(a.RootSpecs(), session.DiscoveryOptions{Agent: a.Agent()})
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
