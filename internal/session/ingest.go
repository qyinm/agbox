package session

import (
	"fmt"
	"time"

	"github.com/hippoom/agbox/internal/store"
)

func IngestOnce(s *store.Store, agent string) (int, error) {
	adapter, ok := ByAgent(agent)
	if !ok {
		return 0, fmt.Errorf("unknown agent: %s", agent)
	}
	sources, err := adapter.DiscoverSources()
	if err != nil {
		return 0, err
	}
	var total int
	for _, src := range sources {
		n, err := ingestSource(s, adapter, src)
		if err != nil {
			return total, err
		}
		total += n
	}
	return total, nil
}

func IngestAll(s *store.Store) (int, error) {
	var total int
	for _, adapter := range All() {
		n, err := IngestOnce(s, adapter.Agent())
		if err != nil {
			return total, err
		}
		total += n
	}
	return total, nil
}

func IngestSource(s *store.Store, adapter Adapter, src Source) error {
	_, err := ingestSource(s, adapter, src)
	return err
}

func ingestSource(s *store.Store, adapter Adapter, src Source) (int, error) {
	row, err := s.GetCursor(src.Path)
	if err != nil {
		return 0, err
	}
	cur := Cursor{
		SourcePath: row.SourcePath,
		LastOffset: row.LastOffset,
		LastHash:   row.LastHash,
	}
	result, err := adapter.ParseDelta(src, cur)
	if err != nil {
		return 0, err
	}
	if err := s.UpsertSession(result.Session); err != nil {
		return 0, err
	}
	if err := s.InsertTurns(result.Turns); err != nil {
		return 0, err
	}
	if err := s.InsertActions(result.Actions); err != nil {
		return 0, err
	}
	for _, c := range result.Corrections {
		if err := s.InsertCorrection(c); err != nil {
			return 0, err
		}
	}
	if err := s.UpsertCursor(store.CursorRow{
		SourcePath:   src.Path,
		Agent:        src.Agent,
		LastOffset:   result.NewOffset,
		LastHash:     result.NewHash,
		LastSyncedAt: time.Now(),
	}); err != nil {
		return 0, err
	}
	return len(result.Corrections), nil
}