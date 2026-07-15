package session

import (
	"errors"
	"os"
	"time"

	"github.com/hippoom/agbox/internal/model"
)

var (
	ErrUnsupportedAdapter    = errors.New("session adapter is not runnable")
	ErrSourceIdentityChanged = errors.New("session source identity changed")
)

const DefaultHistoryWindow = 90 * 24 * time.Hour

type RootClass string

const (
	RootActive  RootClass = "active"
	RootArchive RootClass = "archive"
)

// SessionTimeFunc returns trusted, adapter-specific session time without reading
// transcript contents. The bool is false when the path carries no trustworthy time.
type SessionTimeFunc func(relativePath string, info os.FileInfo) (time.Time, bool)

type RootSpec struct {
	Path         string
	Class        RootClass
	Recursive    bool
	ExcludedDirs []string
	Match        func(relativePath string, entry os.DirEntry) bool
	SessionTime  SessionTimeFunc
}

type DiscoveryOptions struct {
	Agent         string
	Now           time.Time
	HistoryWindow time.Duration
}

type Source struct {
	Agent              string
	Path               string
	Project            string
	RootPath           string
	RootClass          RootClass
	SourceID           string
	Generation         int64
	FileIdentity       string
	Size               int64
	ModTime            time.Time
	SessionTime        time.Time
	HistoricalEligible bool
	BaselineOffset     int64
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

type RootedAdapter interface {
	Adapter
	RootSpecs() []RootSpec
	Runnable() bool
}
