package session

type stubAdapter struct{}

func (stubAdapter) Agent() string { return "stub" }

func (stubAdapter) DiscoverSources() ([]Source, error) { return nil, nil }

func (stubAdapter) ParseDelta(_ Source, _ Cursor) (ParseResult, error) {
	return ParseResult{}, nil
}

var adapters = []Adapter{
	stubAdapter{},
}

func All() []Adapter {
	out := make([]Adapter, len(adapters))
	copy(out, adapters)
	return out
}

func ByAgent(name string) (Adapter, bool) {
	for _, a := range adapters {
		if a.Agent() == name {
			return a, true
		}
	}
	return nil, false
}

func AgentNames() []string {
	names := make([]string, len(adapters))
	for i, a := range adapters {
		names[i] = a.Agent()
	}
	return names
}