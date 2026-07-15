package codex

import (
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"

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
	return "codex"
}

func (a *Adapter) Runnable() bool { return true }

func (a *Adapter) RootSpecs() []session.RootSpec {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil
	}
	matchJSONL := func(rel string, _ os.DirEntry) bool {
		return strings.EqualFold(filepath.Ext(rel), ".jsonl")
	}
	return []session.RootSpec{
		{Path: filepath.Join(home, ".codex", "sessions"), Class: session.RootActive, Recursive: true, Match: matchJSONL, SessionTime: session.DatePathSessionTime},
		{Path: filepath.Join(home, ".codex", "archived_sessions"), Class: session.RootArchive, Recursive: true, Match: matchJSONL, SessionTime: codexArchiveSessionTime},
	}
}

func (a *Adapter) DiscoverSources() ([]session.Source, error) {
	return a.DiscoverSourcesWithOptions(session.DiscoveryOptions{})
}

func (a *Adapter) DiscoverSourcesWithOptions(opts session.DiscoveryOptions) ([]session.Source, error) {
	opts.Agent = a.Agent()
	return session.DiscoverRoots(a.RootSpecs(), opts)
}

var archiveDatePattern = regexp.MustCompile(`(?:^|[^0-9])(20[0-9]{2})-([01][0-9])-([0-3][0-9])(?:[^0-9]|$)`)

func codexArchiveSessionTime(relativePath string, _ os.FileInfo) (time.Time, bool) {
	match := archiveDatePattern.FindStringSubmatch(filepath.Base(relativePath))
	if len(match) != 4 {
		return time.Time{}, false
	}
	parsed, err := time.Parse("2006-01-02", strings.Join(match[1:4], "-"))
	if err != nil {
		return time.Time{}, false
	}
	return parsed, true
}

func (a *Adapter) ParseDelta(src session.Source, cur session.Cursor) (session.ParseResult, error) {
	return session.ParseNative(src, cur, jsonl.CodexHandler{})
}
