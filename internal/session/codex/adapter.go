package codex

import (
	"bufio"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
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
	return "codex"
}

func (a *Adapter) DiscoverSources() ([]session.Source, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, nil
	}
	root := filepath.Join(home, ".codex")
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
		if !strings.HasSuffix(strings.ToLower(fi.Name()), ".jsonl") {
			return nil
		}
		project := filepath.Base(filepath.Dir(path))
		sources = append(sources, session.Source{
			Agent:   "codex",
			Path:    path,
			Project: project,
		})
		return nil
	})
	return sources, nil
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
			for _, block := range rec.Message.Content {
				if block.Type != "tool_use" {
					continue
				}
				turnIndex++
				turn := model.Turn{
					ID:        stableID("turn_", sessionID, fmt.Sprint(turnIndex)),
					SessionID: sessionID,
					TurnIndex: turnIndex,
					Role:      "agent",
					EventType: "tool",
					CreatedAt: now,
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
				result.Turns = append(result.Turns, turn)
				result.Actions = append(result.Actions, action)
				lastAction = &action
			}
		case "user":
			for _, block := range rec.Message.Content {
				if block.Type != "text" || strings.TrimSpace(block.Text) == "" {
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
				raw := privacy.Redact(block.Text)
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
		for _, block := range rec.Message.Content {
			if block.Type != "tool_use" {
				continue
			}
			*turnIndex++
			turn := model.Turn{
				ID:        stableID("turn_", sessionID, fmt.Sprint(*turnIndex)),
				SessionID: sessionID,
				TurnIndex: *turnIndex,
				Role:      "agent",
				EventType: "tool",
				CreatedAt: now,
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
			*lastAction = &action
		}
	case "user":
		for _, block := range rec.Message.Content {
			if block.Type == "text" && strings.TrimSpace(block.Text) != "" {
				*turnIndex++
			}
		}
	}
}

type record struct {
	Type    string  `json:"type"`
	Message message `json:"message"`
}

type message struct {
	Content []contentBlock `json:"content"`
}

type contentBlock struct {
	Type  string         `json:"type"`
	Name  string         `json:"name"`
	Input map[string]any `json:"input"`
	Text  string         `json:"text"`
}

func parseRecord(line string) (record, error) {
	var rec record
	if err := json.Unmarshal([]byte(line), &rec); err != nil {
		return record{}, err
	}
	return rec, nil
}

func extractCommand(input map[string]any) string {
	if input == nil {
		return ""
	}
	if v, ok := input["command"]; ok {
		switch s := v.(type) {
		case string:
			return strings.TrimSpace(s)
		}
	}
	return ""
}

func stableID(prefix string, parts ...string) string {
	sum := sha256.Sum256([]byte(strings.Join(parts, "|")))
	return prefix + hex.EncodeToString(sum[:])[:16]
}

func hashBytes(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}