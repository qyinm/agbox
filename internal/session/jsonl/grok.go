package jsonl

import (
	"encoding/json"
	"fmt"
	"strings"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
)

// GrokHandler parses Grok chat_history.jsonl records.
type GrokHandler struct{}

func (GrokHandler) CapturePaths() CapturePaths {
	return CapturePaths{
		"timestamp":              MaxMetadataBytes,
		"type":                   MaxMetadataBytes,
		"content":                privacy.MaxSignalBytes,
		"content.*.type":         MaxMetadataBytes,
		"content.*.text":         privacy.MaxSignalBytes,
		"tool_calls.*.name":      MaxMetadataBytes,
		"tool_calls.*.arguments": privacy.MaxSignalBytes,
	}
}

func (GrokHandler) ProcessRecord(record Record, ctx *Context, acc *Accum, meta Meta) error {
	createdAt := recordTime(record.First("timestamp"), meta.Now)
	switch record.First("type") {
	case "assistant":
		for _, nameValue := range record.All("tool_calls.*.name") {
			if len(nameValue.Indexes) == 0 {
				continue
			}
			index := nameValue.Indexes[0]
			argumentsValue, _ := record.Captured("tool_calls.*.arguments", index)
			if argumentsValue.Oversized {
				return ErrSignalTooLarge
			}
			arguments := argumentsValue.Value
			ctx.TurnIndex++
			turn := model.Turn{ID: stableID("turn_", meta.SessionID, fmt.Sprint(record.Offset), fmt.Sprint(index)), SessionID: meta.SessionID,
				TurnIndex: ctx.TurnIndex, Role: "agent", EventType: "tool", CreatedAt: createdAt}
			command, filePath := extractGrokToolInput(nameValue.Value, arguments)
			redacted := privacy.Redact(command)
			if redacted == "" && filePath != "" {
				redacted = filePath
			}
			action := model.Action{ID: stableID("act_", turn.ID, nameValue.Value, command, filePath), TurnID: turn.ID,
				ToolName: nameValue.Value, Command: redacted, FilePath: filePath, Excerpt: privacy.Excerpt(redacted, 240)}
			if acc != nil {
				acc.Turns = append(acc.Turns, turn)
				acc.Actions = append(acc.Actions, action)
			}
			ctx.LastAction = &action
			ctx.RequireLastAction = false
		}
	case "user":
		content, _ := record.Captured("content")
		if content.Oversized {
			return ErrSignalTooLarge
		}
		text := content.Value
		if text == "" {
			var parts []string
			for _, blockType := range record.All("content.*.type") {
				if blockType.Value == "text" && len(blockType.Indexes) > 0 {
					textValue, _ := record.Captured("content.*.text", blockType.Indexes[0])
					if textValue.Oversized {
						return ErrSignalTooLarge
					}
					parts = append(parts, textValue.Value)
				}
			}
			text = strings.Join(parts, "\n")
		}
		if strings.TrimSpace(text) == "" {
			return nil
		}
		if ctx.LastAction == nil && ctx.RequireLastAction {
			return ErrMissingContext
		}
		ctx.TurnIndex++
		turn := model.Turn{ID: stableID("turn_", meta.SessionID, fmt.Sprint(record.Offset)), SessionID: meta.SessionID,
			TurnIndex: ctx.TurnIndex, Role: "user", EventType: "message", CreatedAt: createdAt}
		if acc != nil {
			acc.Turns = append(acc.Turns, turn)
			PairCorrection(acc, meta, turn.ID, text, ctx.LastAction, createdAt)
		}
	}
	return nil
}

func (GrokHandler) ApplyContext(line string, ctx *Context) {
	applyGrokLine(line, ctx, nil, Meta{SessionID: "ctx"})
}

func (GrokHandler) ProcessLine(line string, ctx *Context, acc *Accum, meta Meta) error {
	applyGrokLine(line, ctx, acc, meta)
	return nil
}

type grokRecord struct {
	Type      string          `json:"type"`
	Content   json.RawMessage `json:"content"`
	ToolCalls []grokToolCall  `json:"tool_calls"`
}

type grokToolCall struct {
	Name      string `json:"name"`
	Arguments string `json:"arguments"`
}

type grokContentBlock struct {
	Type string `json:"type"`
	Text string `json:"text"`
}

func parseGrokRecord(line string) (grokRecord, error) {
	var rec grokRecord
	if err := json.Unmarshal([]byte(line), &rec); err != nil {
		return grokRecord{}, err
	}
	return rec, nil
}

func applyGrokLine(line string, ctx *Context, acc *Accum, meta Meta) {
	if strings.TrimSpace(line) == "" {
		return
	}
	rec, err := parseGrokRecord(line)
	if err != nil {
		return
	}
	switch rec.Type {
	case "assistant":
		for _, tc := range rec.ToolCalls {
			ctx.TurnIndex++
			turn := model.Turn{
				ID:        stableID("turn_", meta.SessionID, fmt.Sprint(ctx.TurnIndex)),
				SessionID: meta.SessionID,
				TurnIndex: ctx.TurnIndex,
				Role:      "agent",
				EventType: "tool",
				CreatedAt: meta.Now,
			}
			command, filePath := extractGrokToolInput(tc.Name, tc.Arguments)
			redacted := privacy.Redact(command)
			if redacted == "" && filePath != "" {
				redacted = filePath
			}
			action := model.Action{
				ID:       stableID("act_", turn.ID, tc.Name, command, filePath),
				TurnID:   turn.ID,
				ToolName: tc.Name,
				Command:  redacted,
				FilePath: filePath,
				Excerpt:  privacy.Excerpt(redacted, 240),
			}
			if acc != nil {
				acc.Turns = append(acc.Turns, turn)
				acc.Actions = append(acc.Actions, action)
			}
			ctx.LastAction = &action
		}
	case "user":
		text := extractGrokUserText(rec)
		if strings.TrimSpace(text) == "" {
			return
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
			PairCorrection(acc, meta, turn.ID, text, ctx.LastAction, meta.Now)
		}
	}
}

func extractGrokUserText(rec grokRecord) string {
	if len(rec.Content) == 0 {
		return ""
	}
	var text string
	if err := json.Unmarshal(rec.Content, &text); err == nil {
		return text
	}
	var blocks []grokContentBlock
	if err := json.Unmarshal(rec.Content, &blocks); err != nil {
		return ""
	}
	var parts []string
	for _, block := range blocks {
		if block.Type == "text" && strings.TrimSpace(block.Text) != "" {
			parts = append(parts, block.Text)
		}
	}
	return strings.Join(parts, "\n")
}

func extractGrokToolInput(toolName, arguments string) (command, filePath string) {
	if strings.TrimSpace(arguments) == "" {
		return "", ""
	}
	var args map[string]any
	if err := json.Unmarshal([]byte(arguments), &args); err != nil {
		return strings.TrimSpace(arguments), ""
	}
	if v, ok := args["command"]; ok {
		if s, ok := v.(string); ok {
			command = strings.TrimSpace(s)
		}
	}
	for _, key := range []string{"file_path", "path", "filePath"} {
		if v, ok := args[key]; ok {
			if s, ok := v.(string); ok && strings.TrimSpace(s) != "" {
				filePath = strings.TrimSpace(s)
				break
			}
		}
	}
	if command == "" && filePath != "" {
		command = toolName + " " + filePath
	}
	return command, filePath
}
