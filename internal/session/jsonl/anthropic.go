package jsonl

import (
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
)

// AnthropicHandler parses Claude/Codex anthropic-style session jsonl records.
type AnthropicHandler struct{}

func (AnthropicHandler) CapturePaths() CapturePaths {
	return CapturePaths{
		"timestamp":                       MaxMetadataBytes,
		"type":                            MaxMetadataBytes,
		"message.content.*.type":          MaxMetadataBytes,
		"message.content.*.name":          MaxMetadataBytes,
		"message.content.*.input.command": privacy.MaxSignalBytes,
		"message.content.*.text":          privacy.MaxSignalBytes,
	}
}

func (AnthropicHandler) ProcessRecord(record Record, ctx *Context, acc *Accum, meta Meta) error {
	createdAt := recordTime(record.First("timestamp"), meta.Now)
	switch record.First("type") {
	case "assistant":
		for _, blockType := range record.All("message.content.*.type") {
			if blockType.Value != "tool_use" || len(blockType.Indexes) == 0 {
				continue
			}
			index := blockType.Indexes[0]
			name := record.At("message.content.*.name", index)
			command := strings.TrimSpace(record.At("message.content.*.input.command", index))
			ctx.TurnIndex++
			turn := model.Turn{ID: stableID("turn_", meta.SessionID, fmt.Sprint(record.Offset), fmt.Sprint(index)), SessionID: meta.SessionID,
				TurnIndex: ctx.TurnIndex, Role: "agent", EventType: "tool", CreatedAt: createdAt}
			redacted := privacy.Redact(command)
			action := model.Action{ID: stableID("act_", turn.ID, name, command), TurnID: turn.ID, ToolName: name,
				Command: redacted, Excerpt: privacy.Excerpt(redacted, 240)}
			if acc != nil {
				acc.Turns = append(acc.Turns, turn)
				acc.Actions = append(acc.Actions, action)
			}
			ctx.LastAction = &action
			ctx.RequireLastAction = false
		}
	case "user":
		for _, blockType := range record.All("message.content.*.type") {
			if blockType.Value != "text" || len(blockType.Indexes) == 0 {
				continue
			}
			index := blockType.Indexes[0]
			text := record.At("message.content.*.text", index)
			if strings.TrimSpace(text) == "" {
				continue
			}
			if ctx.LastAction == nil && ctx.RequireLastAction {
				return ErrMissingContext
			}
			ctx.TurnIndex++
			turn := model.Turn{ID: stableID("turn_", meta.SessionID, fmt.Sprint(record.Offset), fmt.Sprint(index)), SessionID: meta.SessionID,
				TurnIndex: ctx.TurnIndex, Role: "user", EventType: "message", CreatedAt: createdAt}
			if acc != nil {
				acc.Turns = append(acc.Turns, turn)
				PairCorrection(acc, meta, turn.ID, text, ctx.LastAction)
			}
		}
	}
	return nil
}

func (AnthropicHandler) ApplyContext(line string, ctx *Context) {
	applyAnthropicLine(line, ctx, nil, Meta{SessionID: "ctx"})
}

func (AnthropicHandler) ProcessLine(line string, ctx *Context, acc *Accum, meta Meta) error {
	applyAnthropicLine(line, ctx, acc, meta)
	return nil
}

type anthropicRecord struct {
	Type    string           `json:"type"`
	Message anthropicMessage `json:"message"`
}

type anthropicMessage struct {
	Content []anthropicBlock `json:"content"`
}

type anthropicBlock struct {
	Type  string         `json:"type"`
	Name  string         `json:"name"`
	Input map[string]any `json:"input"`
	Text  string         `json:"text"`
}

func parseAnthropicRecord(line string) (anthropicRecord, error) {
	var rec anthropicRecord
	if err := json.Unmarshal([]byte(line), &rec); err != nil {
		return anthropicRecord{}, err
	}
	return rec, nil
}

func applyAnthropicLine(line string, ctx *Context, acc *Accum, meta Meta) {
	if strings.TrimSpace(line) == "" {
		return
	}
	rec, err := parseAnthropicRecord(line)
	if err != nil {
		return
	}
	switch rec.Type {
	case "assistant":
		for _, block := range rec.Message.Content {
			if block.Type != "tool_use" {
				continue
			}
			ctx.TurnIndex++
			turn := model.Turn{
				ID:        stableID("turn_", meta.SessionID, fmt.Sprint(ctx.TurnIndex)),
				SessionID: meta.SessionID,
				TurnIndex: ctx.TurnIndex,
				Role:      "agent",
				EventType: "tool",
				CreatedAt: meta.Now,
			}
			command := extractCommand(block.Input)
			redacted := privacy.Redact(command)
			action := model.Action{
				ID:       stableID("act_", turn.ID, block.Name, command),
				TurnID:   turn.ID,
				ToolName: block.Name,
				Command:  redacted,
				Excerpt:  privacy.Excerpt(redacted, 240),
			}
			if acc != nil {
				acc.Turns = append(acc.Turns, turn)
				acc.Actions = append(acc.Actions, action)
			}
			ctx.LastAction = &action
		}
	case "user":
		for _, block := range rec.Message.Content {
			if block.Type != "text" || strings.TrimSpace(block.Text) == "" {
				continue
			}
			ctx.TurnIndex++
			turn := model.Turn{
				ID:        stableID("turn_", meta.SessionID, fmt.Sprint(ctx.TurnIndex)),
				SessionID: meta.SessionID,
				TurnIndex: ctx.TurnIndex,
				Role:      "user",
				EventType: "message",
				CreatedAt: meta.Now,
			}
			if acc != nil {
				acc.Turns = append(acc.Turns, turn)
				PairCorrection(acc, meta, turn.ID, block.Text, ctx.LastAction)
			}
		}
	}
}

func extractCommand(input map[string]any) string {
	if input == nil {
		return ""
	}
	if v, ok := input["command"]; ok {
		if s, ok := v.(string); ok {
			return strings.TrimSpace(s)
		}
	}
	return ""
}

func recordTime(raw string, fallback time.Time) time.Time {
	if parsed, err := time.Parse(time.RFC3339Nano, raw); err == nil {
		return parsed
	}
	return fallback
}
