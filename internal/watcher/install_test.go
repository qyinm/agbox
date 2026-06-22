package watcher

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestInstallWritesLaunchAgentPlist(t *testing.T) {
	home := t.TempDir()
	agboxBin := filepath.Join(home, "bin", "agbox")
	if err := os.MkdirAll(filepath.Dir(agboxBin), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(agboxBin, []byte("#!/bin/sh\n"), 0o755); err != nil {
		t.Fatal(err)
	}

	if err := Install(home, agboxBin); err != nil {
		t.Fatal(err)
	}

	plistPath := PlistPath(home)
	data, err := os.ReadFile(plistPath)
	if err != nil {
		t.Fatal(err)
	}
	got := string(data)
	for _, want := range []string{
		Label,
		"watch",
		agboxBin,
		"RunAtLoad",
		"KeepAlive",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("plist missing %q:\n%s", want, got)
		}
	}
}

func TestUninstallRemovesPlist(t *testing.T) {
	home := t.TempDir()
	agboxBin := filepath.Join(home, "agbox")
	if err := os.WriteFile(agboxBin, []byte("#!/bin/sh\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := Install(home, agboxBin); err != nil {
		t.Fatal(err)
	}
	if err := Uninstall(home); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(PlistPath(home)); !os.IsNotExist(err) {
		t.Fatalf("plist still exists: %v", err)
	}
}

func TestStatusReflectsInstalledPlist(t *testing.T) {
	home := t.TempDir()
	agboxBin := filepath.Join(home, "agbox")
	if err := os.WriteFile(agboxBin, []byte("#!/bin/sh\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := Install(home, agboxBin); err != nil {
		t.Fatal(err)
	}

	status := Status(home)
	if !status.Installed {
		t.Fatal("expected installed=true")
	}
	if status.PlistPath != PlistPath(home) {
		t.Fatalf("plist path = %q, want %q", status.PlistPath, PlistPath(home))
	}
	if status.LogPath != LogPath(home) {
		t.Fatalf("log path = %q, want %q", status.LogPath, LogPath(home))
	}
}