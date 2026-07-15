package jsonl

import (
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
)

// CodexHandler parses the native Codex rollout schema. In particular it does
// not treat Codex as Anthropic JSONL: the outer record is response_item or
// event_msg and the agent-native item lives under payload.
type CodexHandler struct{}

func (CodexHandler) CapturePaths() CapturePaths {
	return CapturePaths{
		"timestamp":                MaxMetadataBytes,
		"type":                     MaxMetadataBytes,
		"payload.type":             MaxMetadataBytes,
		"payload.role":             MaxMetadataBytes,
		"payload.name":             MaxMetadataBytes,
		"payload.input":            privacy.MaxSignalBytes,
		"payload.arguments":        privacy.MaxSignalBytes,
		"payload.action.command":   privacy.MaxSignalBytes,
		"payload.action.command.*": privacy.MaxSignalBytes,
		"payload.content.*.type":   MaxMetadataBytes,
		"payload.content.*.text":   privacy.MaxSignalBytes,
		"payload.message":          privacy.MaxSignalBytes,
	}
}

func (CodexHandler) ShouldCapture(path string, partial Record) bool {
	switch path {
	case "payload.content.*.text":
		role := partial.First("payload.role")
		return role == "user" || role == "assistant"
	case "payload.message":
		typeName := partial.First("payload.type")
		return typeName == "user_message" || typeName == "agent_message"
	case "payload.input", "payload.arguments", "payload.action.command", "payload.action.command.*":
		typeName := partial.First("payload.type")
		return typeName == "custom_tool_call" || typeName == "function_call" || typeName == "local_shell_call"
	default:
		return true
	}
}

func (CodexHandler) ProcessRecord(record Record, ctx *Context, acc *Accum, meta Meta) error {
	createdAt := recordTime(record.First("timestamp"), meta.Now)
	switch record.First("type") {
	case "response_item":
		return processCodexResponseItem(record, ctx, acc, meta, createdAt)
	case "event_msg":
		return processCodexEvent(record, ctx, acc, meta, createdAt)
	default:
		return nil
	}
}

func processCodexResponseItem(record Record, ctx *Context, acc *Accum, meta Meta, createdAt time.Time) error {
	payloadType := record.First("payload.type")
	switch payloadType {
	case "custom_tool_call", "function_call", "local_shell_call":
		name := record.First("payload.name")
		if name == "" {
			name = payloadType
		}
		command := record.First("payload.input")
		if command == "" {
			command = commandFromArguments(record.First("payload.arguments"))
		}
		if command == "" {
			command = record.First("payload.action.command")
		}
		if command == "" {
			var parts []string
			for _, value := range record.All("payload.action.command.*") {
				parts = append(parts, value.Value)
			}
			command = strings.Join(parts, " ")
		}
		ctx.TurnIndex++
		turn := model.Turn{ID: stableID("turn_", meta.SessionID, fmt.Sprint(record.Offset), "tool"), SessionID: meta.SessionID,
			TurnIndex: ctx.TurnIndex, Role: "agent", EventType: "tool", CreatedAt: createdAt}
		redacted := privacy.Redact(strings.TrimSpace(command))
		action := model.Action{ID: stableID("act_", turn.ID, name, command), TurnID: turn.ID, ToolName: name,
			Command: redacted, Excerpt: privacy.Excerpt(redacted, 240)}
		if acc != nil {
			acc.Turns = append(acc.Turns, turn)
			acc.Actions = append(acc.Actions, action)
		}
		ctx.LastAction = &action
		ctx.RequireLastAction = false
	case "message":
		role := record.First("payload.role")
		if role != "user" && role != "assistant" {
			return nil
		}
		var parts []string
		for _, blockType := range record.All("payload.content.*.type") {
			if len(blockType.Indexes) == 0 || (blockType.Value != "input_text" && blockType.Value != "output_text" && blockType.Value != "text") {
				continue
			}
			if text := record.At("payload.content.*.text", blockType.Indexes[0]); strings.TrimSpace(text) != "" {
				parts = append(parts, text)
			}
		}
		return appendCodexMessage(record, ctx, acc, meta, role, strings.Join(parts, "\n"), createdAt)
	}
	return nil
}

func processCodexEvent(record Record, ctx *Context, acc *Accum, meta Meta, createdAt time.Time) error {
	switch record.First("payload.type") {
	case "user_message":
		return appendCodexMessage(record, ctx, acc, meta, "user", record.First("payload.message"), createdAt)
	case "agent_message":
		return appendCodexMessage(record, ctx, acc, meta, "assistant", record.First("payload.message"), createdAt)
	default:
		return nil
	}
}

func appendCodexMessage(record Record, ctx *Context, acc *Accum, meta Meta, role, text string, createdAt time.Time) error {
	if strings.TrimSpace(text) == "" {
		return nil
	}
	if role == "user" && ctx.LastAction == nil && ctx.RequireLastAction {
		return ErrMissingContext
	}
	ctx.TurnIndex++
	turn := model.Turn{ID: stableID("turn_", meta.SessionID, fmt.Sprint(record.Offset), role), SessionID: meta.SessionID,
		TurnIndex: ctx.TurnIndex, Role: mapCodexRole(role), EventType: "message", CreatedAt: createdAt}
	if acc != nil {
		acc.Turns = append(acc.Turns, turn)
		if role == "user" {
			PairCorrection(acc, meta, turn.ID, text, ctx.LastAction)
		}
	}
	return nil
}

func mapCodexRole(role string) string {
	if role == "assistant" {
		return "agent"
	}
	return role
}

func commandFromArguments(raw string) string {
	if strings.TrimSpace(raw) == "" {
		return ""
	}
	var fields struct {
		Command string `json:"command"`
		Cmd     string `json:"cmd"`
	}
	if err := json.Unmarshal([]byte(raw), &fields); err != nil {
		return strings.TrimSpace(raw)
	}
	if fields.Command != "" {
		return strings.TrimSpace(fields.Command)
	}
	return strings.TrimSpace(fields.Cmd)
}
