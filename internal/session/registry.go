package session

var adapters []Adapter

// Register adds an adapter. Adapter packages call this from init().
func Register(a Adapter) {
	adapters = append(adapters, a)
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