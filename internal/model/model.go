package model

import "time"

type Event struct {
	ID         string
	Hash       string
	Normalized string
	Source     string
	Agent      string
	Project    string
	Excerpt    string
	Raw        string
	RawStored  bool
	CreatedAt  time.Time
}

type CandidateState string

const (
	CandidatePending  CandidateState = "pending"
	CandidateApproved CandidateState = "approved"
	CandidateRejected CandidateState = "rejected"
	CandidateExported CandidateState = "exported"
)

type Candidate struct {
	ID           string
	Fingerprint  string
	Name         string
	Description  string
	RuleText     string
	State        CandidateState
	EventCount   int
	ProjectCount int
	SourceCount  int
	FirstSeen    time.Time
	LastSeen     time.Time
	Confidence   string
	Version      int
	UpdatedAt    time.Time
}

type EvidenceCard struct {
	Candidate   Candidate
	Sources     []string
	Projects    []string
	Agents      []string
	Excerpts    []string
	Occurrences []Occurrence
	Reason      string
	Privacy     string
}

type ExportStatus string

const (
	ExportPlanned    ExportStatus = "planned"
	ExportApplied    ExportStatus = "applied"
	ExportRolledBack ExportStatus = "rolled_back"
	ExportFailed     ExportStatus = "failed"
)

type ExportRecord struct {
	ID           string
	CandidateID  string
	Target       string
	Path         string
	Status       ExportStatus
	PlanJSON     string
	BackupPath   string
	BeforeHash   string
	AfterHash    string
	AppliedAt    time.Time
	RolledBackAt time.Time
	CreatedAt    time.Time
}

type StoreStats struct {
	Events     int
	Candidates int
	Exports    int
	Path       string
}
