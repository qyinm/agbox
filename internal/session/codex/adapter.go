package codex

import (
	"fmt"
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

	if cur.ParserStateVersion != 0 && cur.ParserStateVersion != jsonl.ContextStateVersion {
		return session.ParseResult{}, fmt.Errorf("%w: version %d", jsonl.ErrParserState, cur.ParserStateVersion)
	}
	identity := src.DurableIdentity()
	sessionID := jsonl.StableID("ses_", src.Agent, identity)
	now := time.Now()
	state := cur.ParserState
	if cur.LastOffset > 0 && len(state) == 0 {
		state = jsonl.MissingContextState()
	}
	stream, err := jsonl.ProcessStream(f, cur.LastOffset, state, jsonl.CodexHandler{}, jsonl.Meta{
		SessionID: sessionID,
		Agent:     src.Agent,
		Project:   src.Project,
		Now:       now,
	})
	if err != nil {
		return session.ParseResult{}, err
	}
	checkpointHash := jsonl.CheckpointHash(identity, stream.NewOffset, stream.ParserState)

	return session.ParseResult{
		Session: model.Session{
			ID:         sessionID,
			Agent:      src.Agent,
			Project:    src.Project,
			SourcePath: src.Path,
			SourceHash: checkpointHash,
			StartedAt:  now,
			UpdatedAt:  now,
		},
		Turns:              stream.Accum.Turns,
		Actions:            stream.Accum.Actions,
		Corrections:        stream.Accum.Corrections,
		NewOffset:          stream.NewOffset,
		NewHash:            checkpointHash,
		ParserStateVersion: stream.ParserStateVersion,
		ParserState:        stream.ParserState,
		BytesRead:          stream.BytesRead,
		Incomplete:         stream.Incomplete,
	}, nil
}
