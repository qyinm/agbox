package grok

import (
	"bufio"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
	"github.com/hippoom/agbox/internal/session"
)

type Adapter struct{}

func New() session.Adapter {
	return &Adapter{}
}

func init() {
	session.Register(New())
}

func (a *Adapter) Agent() string {
	return "grok"
}

func (a *Adapter) DiscoverSources() ([]session.Source, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, nil
	}
	root := filepath.Join(home, ".grok", "sessions")
	info, err := os.Stat(root)
	if err != nil || !info.IsDir() {
		return nil, nil
	}

	var sources []session.Source
	_ = filepath.Walk(root, func(path string, fi os.FileInfo, walkErr error) error {
		if walkErr != nil {
			return nil
		}
		if fi.IsDir() {
			return nil
		}
		if fi.Name() != "chat_history.jsonl" {
			return nil
		}
		project := projectFromPath(path)
		sources = append(sources, session.Source{
			Agent:   "grok",
			Path:    path,
			Project: project,
		})
		return nil
	})
	return sources, nil
}

func projectFromPath(path string) string {
	// ~/.grok/sessions/<encoded-cwd>/<session-id>/chat_history.jsonl
	sessionDir := filepath.Dir(path)
	encodedCWD := filepath.Base(filepath.Dir(sessionDir))
	decoded, err := url.PathUnescape(encodedCWD)
	if err != nil {
		decoded = encodedCWD
	}
	if base := filepath.Base(decoded); base != "" && base != "." && base != "/" {
		return base
	}
	return encodedCWD
}

func (a *Adapter) ParseDelta(src session.Source, cur session.Cursor) (session.ParseResult, error) {
	f, err := os.Open(src.Path)
	if err != nil {
		return session.ParseResult{}, err
	}
	defer f.Close()

	data, err := io.ReadAll(f)
	if err != nil {
		return session.ParseResult{}, err
	}
	fileHash := hashBytes(data)

	sessionID := stableID("ses_", src.Agent, src.Path)
	now := time.Now()

	result := session.ParseResult{
		Session: model.Session{
			ID:         sessionID,
			Agent:      src.Agent,
			Project:    src.Project,
			SourcePath: src.Path,
			SourceHash: fileHash,
			StartedAt:  now,
			UpdatedAt:  now,
		},
		NewOffset: int64(len(data)),
		NewHash:   fileHash,
	}

	var (
		turnIndex  int
		lastAction *model.Action
		offset     int64
	)

	scanner := bufio.NewScanner(strings.NewReader(string(data)))
	for scanner.Scan() {
		lineStart := offset
		line := scanner.Text()
		offset = int64(len(line)) + 1 + lineStart

		if lineStart < cur.LastOffset {
			a.applyLineContext(line, &turnIndex, sessionID, now, &lastAction)
			continue
		}
		if strings.TrimSpace(line) == "" {
			continue
		}

		rec, err := parseRecord(line)
		if err != nil {
			continue
		}

		switch rec.Type {
		case "assistant":
			for _, tc := range rec.ToolCalls {
				turnIndex++
				turn := model.Turn{
					ID:        stableID("turn_", sessionID, fmt.Sprint(turnIndex)),
					SessionID: sessionID,
					TurnIndex: turnIndex,
					Role:      "agent",
					EventType: "tool",
					CreatedAt: now,
				}
				command, filePath := extractToolInput(tc.Name, tc.Arguments)
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
				result.Turns = append(result.Turns, turn)
				result.Actions = append(result.Actions, action)
				lastAction = &action
			}
		case "user":
			text := extractUserText(rec)
			if strings.TrimSpace(text) == "" {
				continue
			}
			turnIndex++
			turn := model.Turn{
				ID:        stableID("turn_", sessionID, fmt.Sprint(turnIndex)),
				SessionID: sessionID,
				TurnIndex: turnIndex,
				Role:      "user",
				EventType: "message",
				CreatedAt: now,
			}
			result.Turns = append(result.Turns, turn)

			if lastAction == nil {
				continue
			}
			raw := privacy.Redact(text)
			normalized := privacy.NormalizeSignal(raw)
			if normalized == "" {
				continue
			}
			sigHash := privacy.HashSignal(normalized)
			correction := model.Correction{
				ID:         stableID("cor_", lastAction.ID, sigHash),
				SessionID:  sessionID,
				TurnID:     turn.ID,
				ActionID:   lastAction.ID,
				Hash:       sigHash,
				Normalized: normalized,
				Excerpt:    privacy.Excerpt(raw, 240),
				Agent:      src.Agent,
				Project:    src.Project,
				CreatedAt:  now,
			}
			result.Corrections = append(result.Corrections, correction)
		}
	}
	if err := scanner.Err(); err != nil {
		return session.ParseResult{}, err
	}

	return result, nil
}

func (a *Adapter) applyLineContext(
	line string,
	turnIndex *int,
	sessionID string,
	now time.Time,
	lastAction **model.Action,
) {
	if strings.TrimSpace(line) == "" {
		return
	}
	rec, err := parseRecord(line)
	if err != nil {
		return
	}
	switch rec.Type {
	case "assistant":
		for _, tc := range rec.ToolCalls {
			*turnIndex++
			turn := model.Turn{
				ID:        stableID("turn_", sessionID, fmt.Sprint(*turnIndex)),
				SessionID: sessionID,
				TurnIndex: *turnIndex,
				Role:      "agent",
				EventType: "tool",
				CreatedAt: now,
			}
			command, filePath := extractToolInput(tc.Name, tc.Arguments)
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
			*lastAction = &action
		}
	case "user":
		if text := extractUserText(rec); strings.TrimSpace(text) != "" {
			*turnIndex++
		}
	}
}

type record struct {
	Type      string        `json:"type"`
	Content   json.RawMessage `json:"content"`
	ToolCalls []toolCall    `json:"tool_calls"`
}

type toolCall struct {
	Name      string `json:"name"`
	Arguments string `json:"arguments"`
}

type contentBlock struct {
	Type string `json:"type"`
	Text string `json:"text"`
}

func parseRecord(line string) (record, error) {
	var rec record
	if err := json.Unmarshal([]byte(line), &rec); err != nil {
		return record{}, err
	}
	return rec, nil
}

func extractUserText(rec record) string {
	if len(rec.Content) == 0 {
		return ""
	}
	var text string
	if err := json.Unmarshal(rec.Content, &text); err == nil {
		return text
	}
	var blocks []contentBlock
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

func extractToolInput(toolName, arguments string) (command, filePath string) {
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

func stableID(prefix string, parts ...string) string {
	sum := sha256.Sum256([]byte(strings.Join(parts, "|")))
	return prefix + hex.EncodeToString(sum[:])[:16]
}

func hashBytes(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}