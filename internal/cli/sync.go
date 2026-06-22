package cli

import (
	"flag"
	"fmt"
	"io"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/store"
)

func runSync(s *store.Store, args []string, stdout io.Writer) error {
	fs := flag.NewFlagSet("sync", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	once := fs.Bool("once", false, "run one ingestion pass and exit")
	if err := fs.Parse(reorderFlags(args, map[string]bool{})); err != nil {
		return err
	}
	if !*once && len(fs.Args()) > 0 {
		return fmt.Errorf("unknown argument %q", fs.Args()[0])
	}
	if !*once {
		*once = true
	}
	n, err := session.IngestAll(s)
	if err != nil {
		return err
	}
	fmt.Fprintf(stdout, "synced %d corrections\n", n)
	return nil
}