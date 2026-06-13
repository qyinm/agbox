package capture

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
	"github.com/hippoom/agbox/internal/store"
)

type Options struct {
	Source    string
	Agent     string
	Project   string
	StoreRaw  bool
	NoExcerpt bool
}

func Capture(s *store.Store, text string, opts Options) (model.Event, error) {
	text = strings.TrimSpace(text)
	if text == "" {
		return model.Event{}, fmt.Errorf("capture text is empty")
	}
	if len([]byte(text)) > privacy.MaxSignalBytes {
		return model.Event{}, fmt.Errorf("capture text exceeds %d bytes", privacy.MaxSignalBytes)
	}
	normalized := privacy.NormalizeSignal(text)
	if normalized == "" {
		return model.Event{}, fmt.Errorf("capture text has no usable workflow signal after normalization")
	}
	now := time.Now()
	hash := privacy.HashSignal(normalized)
	e := model.Event{
		ID:         eventID(hash, now),
		Hash:       hash,
		Normalized: normalized,
		Source:     defaultString(opts.Source, "manual"),
		Agent:      defaultString(opts.Agent, "unknown"),
		Project:    defaultString(opts.Project, "default"),
		CreatedAt:  now,
	}
	if !opts.NoExcerpt {
		e.Excerpt = privacy.Excerpt(text, 240)
	}
	if opts.StoreRaw {
		e.Raw = privacy.Redact(text)
		e.RawStored = true
	}
	if err := s.InsertEvent(e); err != nil {
		return model.Event{}, err
	}
	return e, nil
}

func eventID(hash string, t time.Time) string {
	sum := sha256.Sum256([]byte(fmt.Sprintf("%s:%d", hash, t.UnixNano())))
	return "evt_" + hex.EncodeToString(sum[:])[:16]
}

func defaultString(v, fallback string) string {
	if strings.TrimSpace(v) == "" {
		return fallback
	}
	return strings.TrimSpace(v)
}
