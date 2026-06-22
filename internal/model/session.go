package model

import (
	"fmt"
	"time"
)

type Session struct {
	ID         string
	Agent      string
	Project    string
	SourcePath string
	SourceHash string
	StartedAt  time.Time
	UpdatedAt  time.Time
}

type Turn struct {
	ID        string
	SessionID string
	TurnIndex int
	Role      string // agent | user
	EventType string // message | tool | command
	CreatedAt time.Time
}

type Action struct {
	ID       string
	TurnID   string
	ToolName string
	Command  string
	FilePath string
	Excerpt  string
}

type Correction struct {
	ID         string
	SessionID  string
	TurnID     string
	ActionID   string
	Hash       string
	Normalized string
	Excerpt    string
	Agent      string
	Project    string
	CreatedAt  time.Time
}

type DrillStep struct {
	TurnIndex int
	Role      string
	Summary   string
	CreatedAt time.Time
}

func (s DrillStep) Format() string {
	return fmt.Sprintf("turn %d  %s  %s", s.TurnIndex, s.Role, s.Summary)
}

type Occurrence struct {
	ID             string
	SessionID      string
	CreatedAt      time.Time
	AgentAction    string
	UserCorrection string
	DrillDown      []DrillStep
}

func (o Occurrence) SummaryLine() string {
	return o.AgentAction + "  →  " + o.UserCorrection
}