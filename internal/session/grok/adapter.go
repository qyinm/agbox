package grok

import (
	"net/url"
	"os"
	"path/filepath"

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
	return "grok"
}

func (a *Adapter) Runnable() bool { return true }

func (a *Adapter) RootSpecs() []session.RootSpec {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil
	}
	return []session.RootSpec{{
		Path: filepath.Join(home, ".grok", "sessions"), Class: session.RootActive, Recursive: true,
		Match: func(_ string, entry os.DirEntry) bool { return entry.Name() == "chat_history.jsonl" },
	}}
}

func (a *Adapter) DiscoverSources() ([]session.Source, error) {
	return a.DiscoverSourcesWithOptions(session.DiscoveryOptions{})
}

func (a *Adapter) DiscoverSourcesWithOptions(opts session.DiscoveryOptions) ([]session.Source, error) {
	opts.Agent = a.Agent()
	sources, err := session.DiscoverRoots(a.RootSpecs(), opts)
	for i := range sources {
		sources[i].Project = projectFromPath(sources[i].Path)
	}
	return sources, err
}

func projectFromPath(path string) string {
	sessionDir := filepath.Dir(path)
	encodedCWD := filepath.Base(filepath.Dir(sessionDir))
	decoded, err := url.PathUnescape(encodedCWD)
	if err != nil {
		decoded = encodedCWD
	}
	if base := filepath.Base(decoded); base != "" && base != "." && base != "/" {
		return base
	}
	return encodedCWD
}

func (a *Adapter) ParseDelta(src session.Source, cur session.Cursor) (session.ParseResult, error) {
	return session.ParseNative(src, cur, jsonl.GrokHandler{})
}
