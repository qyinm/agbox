package claude

import (
	"os"
	"path/filepath"
	"strings"

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
	return a.DiscoverSourcesWithOptions(session.DiscoveryOptions{})
}

func (a *Adapter) DiscoverSourcesWithOptions(opts session.DiscoveryOptions) ([]session.Source, error) {
	opts.Agent = a.Agent()
	return session.DiscoverRoots(a.RootSpecs(), opts)
}

func (a *Adapter) ParseDelta(src session.Source, cur session.Cursor) (session.ParseResult, error) {
	return session.ParseNative(src, cur, jsonl.AnthropicHandler{})
}
