package codex_test

import (
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/hippoom/agbox/internal/privacy"
	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/session/codex"
	"github.com/hippoom/agbox/internal/session/jsonl"
)

func testdataPath(t *testing.T, name string) string {
	t.Helper()
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	return filepath.Join(filepath.Dir(file), "testdata", name)
}

func TestParseDeltaDetectsCorrection(t *testing.T) {
	adapter := codex.New()
	src := session.Source{Agent: "codex", Path: testdataPath(t, "sample.jsonl"), Project: "demo"}
	result, err := adapter.ParseDelta(src, session.Cursor{})
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Corrections) != 1 {
		t.Fatalf("corrections = %d, want 1", len(result.Corrections))
	}
	if result.Corrections[0].Excerpt == "" {
		t.Fatal("expected redacted excerpt")
	}
}

func TestParseDeltaPreservesActionLinkageAcrossRestart(t *testing.T) {
	path := filepath.Join(t.TempDir(), "rollout.jsonl")
	action := "{\"timestamp\":\"2026-07-15T15:32:30Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"name\":\"exec\",\"input\":\"npm install\"}}\n"
	if err := os.WriteFile(path, []byte(action), 0o600); err != nil {
		t.Fatal(err)
	}
	adapter := codex.New()
	src := session.Source{Agent: "codex", Path: path, Project: "demo", SourceID: "stable-source"}
	first, err := adapter.ParseDelta(src, session.Cursor{})
	if err != nil {
		t.Fatal(err)
	}
	if len(first.Actions) != 1 || len(first.ParserState) == 0 {
		t.Fatalf("first parse actions/state = %d/%d", len(first.Actions), len(first.ParserState))
	}
	correction := "{\"timestamp\":\"2026-07-15T15:32:31Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"use bun\"}}\n"
	f, err := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.WriteString(correction); err != nil {
		f.Close()
		t.Fatal(err)
	}
	f.Close()
	second, err := adapter.ParseDelta(src, session.Cursor{LastOffset: first.NewOffset, ParserStateVersion: first.ParserStateVersion, ParserState: first.ParserState})
	if err != nil {
		t.Fatal(err)
	}
	if second.BytesRead != int64(len(correction)) {
		t.Fatalf("bytes read = %d, want appended %d", second.BytesRead, len(correction))
	}
	if len(second.Corrections) != 1 || second.Corrections[0].ActionID != first.Actions[0].ID {
		t.Fatalf("restart linkage lost: first=%+v second=%+v", first.Actions, second.Corrections)
	}

	atEOF, err := adapter.ParseDelta(src, session.Cursor{LastOffset: second.NewOffset, ParserStateVersion: second.ParserStateVersion, ParserState: second.ParserState})
	if err != nil {
		t.Fatal(err)
	}
	if atEOF.BytesRead != 0 || len(atEOF.Turns) != 0 {
		t.Fatalf("EOF parse read/replayed content: %+v", atEOF)
	}
}

func TestStableSourceIDMakesResultsPathIndependent(t *testing.T) {
	data, err := os.ReadFile(testdataPath(t, "sample.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	paths := []string{filepath.Join(t.TempDir(), "a.jsonl"), filepath.Join(t.TempDir(), "b.jsonl")}
	var ids []string
	for _, path := range paths {
		if err := os.WriteFile(path, data, 0o600); err != nil {
			t.Fatal(err)
		}
		result, err := codex.New().ParseDelta(session.Source{Agent: "codex", Path: path, SourceID: "same-source", Generation: 7}, session.Cursor{})
		if err != nil {
			t.Fatal(err)
		}
		ids = append(ids, result.Corrections[0].ID)
	}
	if ids[0] != ids[1] {
		t.Fatalf("correction IDs vary by path: %v", ids)
	}
}

func TestStableIDsSeparateSourceGenerationsAndRecordOrdinals(t *testing.T) {
	data := "{\"timestamp\":\"2026-07-15T15:32:30Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"name\":\"exec\",\"input\":\"npm install\"}}\n" +
		"{\"timestamp\":\"2026-07-15T15:32:31Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"use bun\"}}\n" +
		"{\"timestamp\":\"2026-07-15T15:32:32Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"use bun\"}}\n"
	path := filepath.Join(t.TempDir(), "rollout.jsonl")
	if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
		t.Fatal(err)
	}
	parse := func(generation int64) session.ParseResult {
		t.Helper()
		result, err := codex.New().ParseDelta(session.Source{
			Agent: "codex", Path: path, SourceID: "same-source", Generation: generation,
		}, session.Cursor{})
		if err != nil {
			t.Fatal(err)
		}
		return result
	}
	first := parse(1)
	second := parse(2)
	if len(first.Corrections) != 2 {
		t.Fatalf("same text at distinct record positions produced %d corrections, want 2", len(first.Corrections))
	}
	if first.Corrections[0].ID == first.Corrections[1].ID {
		t.Fatalf("distinct record ordinals shared correction ID %q", first.Corrections[0].ID)
	}
	if first.Session.ID == second.Session.ID || first.Turns[0].ID == second.Turns[0].ID || first.Corrections[0].ID == second.Corrections[0].ID {
		t.Fatal("replacement generation reused durable identities")
	}
}

func TestStableIDsAreIdenticalAcrossSliceBoundaryAndRestart(t *testing.T) {
	action := "{\"timestamp\":\"2026-07-15T15:32:30Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"name\":\"exec\",\"input\":\"npm install\"}}\n"
	correction := "{\"timestamp\":\"2026-07-15T15:32:31Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"use bun\"}}\n"
	dir := t.TempDir()
	fullPath := filepath.Join(dir, "full.jsonl")
	splitPath := filepath.Join(dir, "split.jsonl")
	if err := os.WriteFile(fullPath, []byte(action+correction), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(splitPath, []byte(action), 0o600); err != nil {
		t.Fatal(err)
	}
	adapter := codex.New()
	full, err := adapter.ParseDelta(session.Source{Agent: "codex", Path: fullPath, SourceID: "stable", Generation: 3}, session.Cursor{})
	if err != nil {
		t.Fatal(err)
	}
	first, err := adapter.ParseDelta(session.Source{Agent: "codex", Path: splitPath, SourceID: "stable", Generation: 3}, session.Cursor{})
	if err != nil {
		t.Fatal(err)
	}
	f, err := os.OpenFile(splitPath, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.WriteString(correction); err != nil {
		f.Close()
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
	second, err := adapter.ParseDelta(session.Source{Agent: "codex", Path: splitPath, SourceID: "stable", Generation: 3}, session.Cursor{
		LastOffset: first.NewOffset, ParserStateVersion: first.ParserStateVersion, ParserState: first.ParserState,
	})
	if err != nil {
		t.Fatal(err)
	}
	if full.Session.ID != first.Session.ID || full.Actions[0].ID != first.Actions[0].ID ||
		full.Corrections[0].ID != second.Corrections[0].ID || full.Corrections[0].TurnID != second.Corrections[0].TurnID {
		t.Fatalf("slice/restart identities differ: full=%+v/%+v split=%+v/%+v", full.Actions, full.Corrections, first.Actions, second.Corrections)
	}
}

func TestOldEOFBaselineSignalsMissingCorrectionContext(t *testing.T) {
	path := filepath.Join(t.TempDir(), "rollout.jsonl")
	oldAction := "{\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"name\":\"exec\",\"input\":\"npm install\"}}\n"
	newCorrection := "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"use bun\"}}\n"
	if err := os.WriteFile(path, []byte(oldAction+newCorrection), 0o600); err != nil {
		t.Fatal(err)
	}
	_, err := codex.New().ParseDelta(session.Source{Agent: "codex", Path: path, SourceID: "baseline"}, session.Cursor{LastOffset: int64(len(oldAction))})
	if !errors.Is(err, jsonl.ErrMissingContext) {
		t.Fatalf("error = %v, want diagnosable missing-context quarantine signal", err)
	}
}

func TestCaptureIsIndependentOfJSONKeyOrder(t *testing.T) {
	path := filepath.Join(t.TempDir(), "rollout.jsonl")
	data := "{\"type\":\"response_item\",\"payload\":{\"input\":\"npm install\",\"name\":\"exec\",\"type\":\"custom_tool_call\"}}\n" +
		"{\"type\":\"event_msg\",\"payload\":{\"message\":\"use bun\",\"type\":\"user_message\"}}\n"
	if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
		t.Fatal(err)
	}
	result, err := codex.New().ParseDelta(session.Source{Agent: "codex", Path: path, SourceID: "ordered"}, session.Cursor{})
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Actions) != 1 || len(result.Corrections) != 1 {
		t.Fatalf("key-order parse actions/corrections = %d/%d, want 1/1", len(result.Actions), len(result.Corrections))
	}
}

func TestOversizedIrrelevantAssistantMessageDoesNotQuarantineSource(t *testing.T) {
	path := filepath.Join(t.TempDir(), "rollout.jsonl")
	data := "{\"type\":\"event_msg\",\"payload\":{\"message\":\"" + strings.Repeat("x", privacy.MaxSignalBytes+1) + "\",\"type\":\"agent_message\"}}\n" +
		"{\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"name\":\"exec\",\"input\":\"npm install\"}}\n" +
		"{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"use bun\"}}\n"
	if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
		t.Fatal(err)
	}
	result, err := codex.New().ParseDelta(session.Source{Agent: "codex", Path: path, SourceID: "irrelevant"}, session.Cursor{})
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Corrections) != 1 {
		t.Fatalf("corrections = %d, want 1 after irrelevant oversized record", len(result.Corrections))
	}
}
