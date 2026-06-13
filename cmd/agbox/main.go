package main

import (
	"os"

	"github.com/hippoom/agbox/internal/cli"
)

func main() {
	if err := cli.Execute(os.Args[1:], os.Stdin, os.Stdout, os.Stderr); err != nil {
		cli.PrintError(os.Stderr, err)
		os.Exit(1)
	}
}
