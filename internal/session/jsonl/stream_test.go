package jsonl

import (
	"bytes"
	"errors"
	"io"
	"strings"
	"testing"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
)

type testNativeHandler struct{}

func (testNativeHandler) CapturePaths() CapturePaths {
	return CapturePaths{"type": MaxMetadataBytes, "text": privacy.MaxSignalBytes}
}

func (testNativeHandler) ProcessRecord(record Record, _ *Context, acc *Accum, _ Meta) error {
	if record.First("type") == "signal" {
		acc.Corrections = append(acc.Corrections, correctionForTest(record.First("text")))
	}
	return nil
}

func correctionForTest(text string) (out model.Correction) {
	out.Excerpt = text
	return out
}

type countedReadSeeker struct {
	*bytes.Reader
	read       int64
	maxRequest int
}

func (r *countedReadSeeker) Read(p []byte) (int, error) {
	if len(p) > r.maxRequest {
		r.maxRequest = len(p)
	}
	n, err := r.Reader.Read(p)
	r.read += int64(n)
	return n, err
}

func TestProcessStreamAtEOFReadsNoContent(t *testing.T) {
	data := []byte("{\"type\":\"ignore\"}\n")
	r := &countedReadSeeker{Reader: bytes.NewReader(data)}
	result, err := ProcessStream(r, int64(len(data)), nil, testNativeHandler{}, Meta{})
	if err != nil {
		t.Fatal(err)
	}
	if result.NewOffset != int64(len(data)) || r.read != 0 {
		t.Fatalf("offset/read = %d/%d, want %d/0", result.NewOffset, r.read, len(data))
	}
}

func TestProcessStreamIncompleteTrailingRecordDoesNotAdvance(t *testing.T) {
	prefix := []byte("{\"type\":\"ignore\"}\n")
	data := append(append([]byte{}, prefix...), []byte("{\"type\":\"signal\",\"text\":\"later")...)
	result, err := ProcessStream(bytes.NewReader(data), 0, nil, testNativeHandler{}, Meta{})
	if err != nil {
		t.Fatal(err)
	}
	if result.NewOffset != int64(len(prefix)) || !result.Incomplete {
		t.Fatalf("result = %+v, want safe offset %d and incomplete", result, len(prefix))
	}
}

func TestProcessStreamSkipsLargeIrrelevantStringWithoutLargeAllocation(t *testing.T) {
	data := []byte(`{"type":"ignore","blob":"` + strings.Repeat("x", 32<<20) + `"}` + "\n")
	r := &countedReadSeeker{Reader: bytes.NewReader(data)}
	result, err := ProcessStream(r, 0, nil, testNativeHandler{}, Meta{})
	if err != nil {
		t.Fatal(err)
	}
	if result.NewOffset != int64(len(data)) {
		t.Fatalf("offset = %d, want %d", result.NewOffset, len(data))
	}
	if r.maxRequest > 32<<10 {
		t.Fatalf("largest read buffer = %d, want <= %d", r.maxRequest, 32<<10)
	}
}

func TestProcessStreamRejectsOversizedRelevantTextWithoutAdvance(t *testing.T) {
	data := []byte(`{"type":"signal","text":"` + strings.Repeat("x", privacy.MaxSignalBytes+1) + `"}` + "\n")
	result, err := ProcessStream(bytes.NewReader(data), 0, nil, testNativeHandler{}, Meta{})
	if !errors.Is(err, ErrSignalTooLarge) {
		t.Fatalf("error = %v, want ErrSignalTooLarge", err)
	}
	if result.NewOffset != 0 {
		t.Fatalf("offset = %d, want no advancement", result.NewOffset)
	}
}

func TestProcessStreamMalformedRecordDoesNotAdvance(t *testing.T) {
	result, err := ProcessStream(bytes.NewReader([]byte("{not-json}\n")), 0, nil, testNativeHandler{}, Meta{})
	if !errors.Is(err, ErrMalformedRecord) {
		t.Fatalf("error = %v, want ErrMalformedRecord", err)
	}
	if result.NewOffset != 0 {
		t.Fatalf("offset = %d, want no advancement", result.NewOffset)
	}
}

func TestProcessStreamRejectsDepthAndTokenAmplification(t *testing.T) {
	deep := strings.Repeat("[", MaxJSONDepth+2) + "null" + strings.Repeat("]", MaxJSONDepth+2) + "\n"
	if _, err := ProcessStream(bytes.NewReader([]byte(deep)), 0, nil, testNativeHandler{}, Meta{}); !errors.Is(err, ErrRecordBudget) {
		t.Fatalf("deep error = %v, want ErrRecordBudget", err)
	}

	tokens := "[" + strings.Repeat("null,", MaxJSONTokens) + "null]\n"
	if _, err := ProcessStream(bytes.NewReader([]byte(tokens)), 0, nil, testNativeHandler{}, Meta{}); !errors.Is(err, ErrRecordBudget) {
		t.Fatalf("token error = %v, want ErrRecordBudget", err)
	}
}

var _ io.ReadSeeker = (*countedReadSeeker)(nil)
