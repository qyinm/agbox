package jsonl

import (
	"strings"
	"testing"
)

type recordingHandler struct {
	contextLines int
	processLines int
}

func (h *recordingHandler) ApplyContext(line string, ctx *Context) {
	h.contextLines++
}

func (h *recordingHandler) ProcessLine(line string, ctx *Context, acc *Accum, meta Meta) error {
	h.processLines++
	return nil
}

func TestProcessDeltaHandlesOversizedLine(t *testing.T) {
	line := strings.Repeat("x", 70*1024)
	data := []byte(line + "\n")
	handler := &recordingHandler{}
	_, offset, err := ProcessDelta(data, 0, handler, Meta{})
	if err != nil {
		t.Fatal(err)
	}
	if handler.processLines != 1 {
		t.Fatalf("processed lines = %d, want 1", handler.processLines)
	}
	if offset != int64(len(data)) {
		t.Fatalf("offset = %d, want %d", offset, len(data))
	}
}
