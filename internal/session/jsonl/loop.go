package jsonl

import (
	"bufio"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
)

// Context tracks turn state while scanning a session jsonl file.
type Context struct {
	TurnIndex  int
	LastAction *model.Action
}

// Accum collects parsed session entities from jsonl deltas.
type Accum struct {
	Turns       []model.Turn
	Actions     []model.Action
	Corrections []model.Correction
}

// LineHandler parses one jsonl line into turns/actions/corrections.
type LineHandler interface {
	ApplyContext(line string, ctx *Context)
	ProcessLine(line string, ctx *Context, acc *Accum, meta Meta) error
}

// Meta is stable session metadata passed to each line handler.
type Meta struct {
	SessionID string
	Agent     string
	Project   string
	Now       time.Time
}

// ProcessDelta scans jsonl content from lastOffset, invoking handler per line.
func ProcessDelta(data []byte, lastOffset int64, handler LineHandler, meta Meta) (Accum, int64, error) {
	var acc Accum
	ctx := &Context{}
	var offset int64
	scanner := bufio.NewScanner(strings.NewReader(string(data)))
	for scanner.Scan() {
		lineStart := offset
		line := scanner.Text()
		offset = int64(len(line)) + 1 + lineStart
		if lineStart < lastOffset {
			handler.ApplyContext(line, ctx)
			continue
		}
		if strings.TrimSpace(line) == "" {
			continue
		}
		if err := handler.ProcessLine(line, ctx, &acc, meta); err != nil {
			return acc, offset, err
		}
	}
	return acc, offset, scanner.Err()
}

// PairCorrection links a user correction to the most recent agent action.
func PairCorrection(acc *Accum, meta Meta, turnID, raw string, lastAction *model.Action) {
	if lastAction == nil {
		return
	}
	normalized := normalize(raw)
	if normalized == "" {
		return
	}
	sigHash := hashSignal(normalized)
	acc.Corrections = append(acc.Corrections, model.Correction{
		ID:         stableID("cor_", lastAction.ID, sigHash),
		SessionID:  meta.SessionID,
		TurnID:     turnID,
		ActionID:   lastAction.ID,
		Hash:       sigHash,
		Normalized: normalized,
		Excerpt:    excerpt(raw, 240),
		Agent:      meta.Agent,
		Project:    meta.Project,
		CreatedAt:  meta.Now,
	})
}