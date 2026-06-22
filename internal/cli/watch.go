package cli

import (
	"context"
	"os"
	"os/signal"
	"syscall"

	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/watcher"
)

func runWatch() error {
	s, err := store.Open("")
	if err != nil {
		return err
	}
	defer s.Close()

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	return watcher.Run(ctx, s, watcher.DefaultPollInterval)
}