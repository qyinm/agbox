package codex

import (
	"io"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
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
	return session.DiscoverRoots(a.RootSpecs(), session.DiscoveryOptions{Agent: a.Agent()})
}

var archiveDatePattern = regexp.MustCompile(`(?:^|[^0-9])(20[0-9]{2})-([01][0-9])-([0-3][0-9])(?:[^0-9]|$)`)

func codexArchiveSessionTime(relativePath string, _ os.FileInfo) (time.Time, bool) {
	match := archiveDatePattern.FindStringSubmatch(filepath.Base(relativePath))
	if len(match) != 4 {
		return time.Time{}, false
	}
	y, errY := strconv.Atoi(match[1])
	m, errM := strconv.Atoi(match[2])
	d, errD := strconv.Atoi(match[3])
	if errY != nil || errM != nil || errD != nil || m < 1 || m > 12 || d < 1 || d > 31 {
		return time.Time{}, false
	}
	parsed := time.Date(y, time.Month(m), d, 0, 0, 0, 0, time.UTC)
	if parsed.Year() != y || int(parsed.Month()) != m || parsed.Day() != d {
		return time.Time{}, false
	}
	return parsed, true
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
