package session

import "github.com/hippoom/agbox/internal/model"

type Source struct {
	Agent   string
	Path    string
	Project string
}

type Cursor struct {
	SourcePath string
	LastOffset int64
	LastHash   string
}

type ParseResult struct {
	Session     model.Session
	Turns       []model.Turn
	Actions     []model.Action
	Corrections []model.Correction
	NewOffset   int64
	NewHash     string
}

type Adapter interface {
	Agent() string
	DiscoverSources() ([]Source, error)
	ParseDelta(src Source, cur Cursor) (ParseResult, error)
}