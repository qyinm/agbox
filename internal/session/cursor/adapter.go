package cursor

import "github.com/hippoom/agbox/internal/session"

type Adapter struct{}

func New() *Adapter {
	return &Adapter{}
}

func init() {
	session.Register(New())
}

func (a *Adapter) Agent() string {
	return "cursor"
}

func (a *Adapter) Runnable() bool { return false }

func (a *Adapter) RootSpecs() []session.RootSpec { return nil }

func (a *Adapter) DiscoverSources() ([]session.Source, error) {
	return nil, nil
}

func (a *Adapter) ParseDelta(src session.Source, _ session.Cursor) (session.ParseResult, error) {
	return session.ParseResult{}, session.ErrUnsupportedAdapter
}
