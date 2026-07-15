package claude

import (
	"fmt"
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
	return a.DiscoverSourcesWithOptions(session.DiscoveryOptions{})
}

func (a *Adapter) DiscoverSourcesWithOptions(opts session.DiscoveryOptions) ([]session.Source, error) {
	opts.Agent = a.Agent()
	return session.DiscoverRoots(a.RootSpecs(), opts)
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
	stream, err := jsonl.ProcessStream(f, cur.LastOffset, state, jsonl.AnthropicHandler{}, jsonl.Meta{
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
