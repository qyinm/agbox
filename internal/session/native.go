package session

import (
	"fmt"
	"io"
	"os"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/session/jsonl"
)

// ParseNative streams a discovered JSONL source through an agent-specific
// handler while preserving the shared checkpoint and identity contract.
func ParseNative(src Source, cur Cursor, handler jsonl.NativeHandler) (ParseResult, error) {
	var (
		f   *os.File
		err error
	)
	if src.RootPath != "" && src.FileIdentity != "" {
		f, err = VerifiedOpen(src)
	} else {
		// Synthetic/demo callers predate durable discovery metadata. Production
		// discovery always supplies the fields required by VerifiedOpen.
		f, err = os.Open(src.Path)
	}
	if err != nil {
		return ParseResult{}, err
	}
	defer f.Close()

	if cur.ParserStateVersion != 0 && cur.ParserStateVersion != jsonl.ContextStateVersion {
		return ParseResult{}, fmt.Errorf("%w: version %d", jsonl.ErrParserState, cur.ParserStateVersion)
	}
	identity := src.DurableIdentity()
	sessionID := jsonl.StableID("ses_", src.Agent, identity)
	now := time.Now()
	state := cur.ParserState
	if cur.LastOffset > 0 && len(state) == 0 {
		state = jsonl.MissingContextState()
	}
	var source io.ReadSeeker = f
	if src.Size > 0 {
		source = io.NewSectionReader(f, 0, src.Size)
	}
	stream, err := jsonl.ProcessStream(source, cur.LastOffset, state, handler, jsonl.Meta{
		SessionID: sessionID,
		Agent:     src.Agent,
		Project:   src.Project,
		Now:       now,
	})
	if err != nil {
		return ParseResult{}, err
	}
	checkpointHash := jsonl.CheckpointHash(identity, stream.NewOffset, stream.ParserState)

	return ParseResult{
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
