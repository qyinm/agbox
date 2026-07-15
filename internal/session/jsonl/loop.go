package jsonl

import (
	"bufio"
	"bytes"
	"errors"
	"io"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
)

// Context tracks turn state while scanning a session jsonl file.
type Context struct {
	TurnIndex         int
	LastAction        *model.Action
	RequireLastAction bool
	LastUserSignature string
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
	reader := bufio.NewReader(bytes.NewReader(data))
	for {
		raw, err := reader.ReadString('\n')
		if err != nil && len(raw) == 0 {
			if errors.Is(err, io.EOF) {
				return acc, offset, nil
			}
			return acc, offset, err
		}
		lineStart := offset
		offset += int64(len(raw))
		line := strings.TrimSuffix(strings.TrimSuffix(raw, "\n"), "\r")
		if lineStart < lastOffset {
			handler.ApplyContext(line, ctx)
		} else if strings.TrimSpace(line) != "" {
			if err := handler.ProcessLine(line, ctx, &acc, meta); err != nil {
				return acc, offset, err
			}
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				return acc, offset, nil
			}
			return acc, offset, err
		}
	}
}

// PairCorrection links a user correction to the most recent agent action.
func PairCorrection(acc *Accum, meta Meta, turnID, raw string, lastAction *model.Action, createdAt time.Time) {
	if lastAction == nil {
		return
	}
	normalized := normalize(raw)
	if normalized == "" {
		return
	}
	if createdAt.IsZero() {
		createdAt = meta.Now
	}
	sigHash := hashSignal(normalized)
	// The correction turn carries the record position (and record-local ordinal)
	// assigned by the native handler. Include it so identical correction text
	// repeated after one action remains two durable source records while retries
	// of either record remain idempotent.
	id := stableID("cor_", lastAction.ID, turnID, sigHash)
	for _, existing := range acc.Corrections {
		if existing.ID == id {
			return
		}
	}
	acc.Corrections = append(acc.Corrections, model.Correction{
		ID:         id,
		SessionID:  meta.SessionID,
		TurnID:     turnID,
		ActionID:   lastAction.ID,
		Hash:       sigHash,
		Normalized: normalized,
		Excerpt:    excerpt(raw, 240),
		Agent:      meta.Agent,
		Project:    meta.Project,
		CreatedAt:  createdAt,
	})
}
