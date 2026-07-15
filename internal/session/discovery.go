package session

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"
)

type Reconciliation struct {
	Current  []Source
	Deleted  []Source
	Replaced []Source
}

func DiscoverRoots(specs []RootSpec, opts DiscoveryOptions) ([]Source, error) {
	var out []Source
	var errs []error
	for _, spec := range specs {
		sources, err := DiscoverRoot(spec, opts)
		if err != nil {
			errs = append(errs, err)
			continue
		}
		out = append(out, sources...)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Path < out[j].Path })
	return out, errors.Join(errs...)
}

func DiscoverRoot(spec RootSpec, opts DiscoveryOptions) ([]Source, error) {
	if spec.Path == "" {
		return nil, nil
	}
	root, err := filepath.Abs(spec.Path)
	if err != nil {
		return nil, err
	}
	root, err = filepath.EvalSymlinks(root)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	rootInfo, err := os.Lstat(root)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil || !rootInfo.IsDir() {
		return nil, err
	}
	if opts.Now.IsZero() {
		opts.Now = time.Now()
	}
	if opts.HistoryWindow <= 0 {
		opts.HistoryWindow = DefaultHistoryWindow
	}
	excluded := defaultExcludedDirs()
	for _, name := range spec.ExcludedDirs {
		excluded[strings.ToLower(name)] = struct{}{}
	}
	var sources []Source
	err = filepath.WalkDir(root, func(path string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return nil // isolate unreadable entries
		}
		rel, err := filepath.Rel(root, path)
		if err != nil || rel == "." {
			return nil
		}
		if entry.IsDir() {
			if _, skip := excluded[strings.ToLower(entry.Name())]; skip {
				return filepath.SkipDir
			}
			if !spec.Recursive && filepath.Dir(rel) == "." {
				return filepath.SkipDir
			}
			return nil
		}
		if entry.Type()&os.ModeSymlink != 0 || !entry.Type().IsRegular() {
			return nil
		}
		if spec.Match != nil && !spec.Match(rel, entry) {
			return nil
		}
		info, err := entry.Info()
		if err != nil || !info.Mode().IsRegular() {
			return nil
		}
		if err := rejectSymlinkComponents(root, path); err != nil {
			return nil
		}
		identity := statIdentity(info)
		if identity == "" {
			return nil
		}
		sessionTime, trusted := time.Time{}, false
		if spec.SessionTime != nil {
			sessionTime, trusted = spec.SessionTime(rel, info)
		}
		eligible := trusted && !sessionTime.Before(opts.Now.Add(-opts.HistoryWindow)) && !sessionTime.After(opts.Now.Add(24*time.Hour))
		baseline := int64(0)
		if spec.Class == RootActive && !eligible {
			baseline = info.Size()
		}
		sources = append(sources, Source{
			Agent: opts.Agent, Path: path, Project: filepath.Base(filepath.Dir(path)),
			RootPath: root, RootClass: spec.Class, SourceID: opaqueSourceID(opts.Agent, identity),
			Generation: 1, FileIdentity: identity, Size: info.Size(), ModTime: info.ModTime(),
			SessionTime: sessionTime, HistoricalEligible: eligible, BaselineOffset: baseline,
		})
		return nil
	})
	return sources, err
}

func DatePathSessionTime(relativePath string, _ os.FileInfo) (time.Time, bool) {
	parts := strings.Split(filepath.ToSlash(relativePath), "/")
	for i := 0; i+2 < len(parts); i++ {
		y, errY := strconv.Atoi(parts[i])
		m, errM := strconv.Atoi(parts[i+1])
		d, errD := strconv.Atoi(parts[i+2])
		if errY == nil && errM == nil && errD == nil && y >= 2000 && m >= 1 && m <= 12 && d >= 1 && d <= 31 {
			parsed := time.Date(y, time.Month(m), d, 0, 0, 0, 0, time.UTC)
			if parsed.Year() == y && int(parsed.Month()) == m && parsed.Day() == d {
				return parsed, true
			}
		}
	}
	return time.Time{}, false
}

func VerifiedOpen(src Source) (*os.File, error) {
	if src.RootPath == "" || src.Path == "" || rejectSymlinkComponents(src.RootPath, src.Path) != nil {
		return nil, ErrSourceIdentityChanged
	}
	fd, err := syscall.Open(src.Path, syscall.O_RDONLY|syscall.O_NOFOLLOW, 0)
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrSourceIdentityChanged, err)
	}
	f := os.NewFile(uintptr(fd), src.Path)
	info, err := f.Stat()
	if err != nil || !info.Mode().IsRegular() || statIdentity(info) != src.FileIdentity {
		f.Close()
		return nil, ErrSourceIdentityChanged
	}
	return f, nil
}

func RefreshSource(src Source) (Source, error) {
	if src.RootPath == "" || src.Path == "" || rejectSymlinkComponents(src.RootPath, src.Path) != nil {
		return Source{}, ErrSourceIdentityChanged
	}
	info, err := os.Lstat(src.Path)
	if err != nil || !info.Mode().IsRegular() || statIdentity(info) != src.FileIdentity {
		return Source{}, ErrSourceIdentityChanged
	}
	src.Size = info.Size()
	src.ModTime = info.ModTime()
	return src, nil
}

func ReconcileSources(previous, observed []Source) Reconciliation {
	prevByIdentity := make(map[string]Source, len(previous))
	prevByPath := make(map[string]Source, len(previous))
	for _, src := range previous {
		prevByIdentity[src.FileIdentity] = src
		prevByPath[src.Path] = src
	}
	used := make(map[string]bool, len(previous))
	result := Reconciliation{Current: make([]Source, 0, len(observed))}
	for _, src := range observed {
		if prev, ok := prevByIdentity[src.FileIdentity]; ok {
			src.SourceID, src.Generation = prev.SourceID, prev.Generation
			used[prev.Path] = true
			if src.Size < prev.Size {
				src.Generation++
				result.Replaced = append(result.Replaced, prev)
			} else {
				src.BaselineOffset = prev.BaselineOffset
			}
		} else if prev, ok := prevByPath[src.Path]; ok {
			src.SourceID, src.Generation = prev.SourceID, prev.Generation+1
			used[prev.Path] = true
			result.Replaced = append(result.Replaced, prev)
		}
		result.Current = append(result.Current, src)
	}
	for _, prev := range previous {
		if !used[prev.Path] {
			result.Deleted = append(result.Deleted, prev)
		}
	}
	return result
}

func rejectSymlinkComponents(root, path string) error {
	root, err := filepath.Abs(root)
	if err != nil {
		return err
	}
	path, err = filepath.Abs(path)
	if err != nil {
		return err
	}
	rel, err := filepath.Rel(root, path)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return ErrSourceIdentityChanged
	}
	current := root
	for _, part := range strings.Split(rel, string(filepath.Separator)) {
		current = filepath.Join(current, part)
		info, err := os.Lstat(current)
		if err != nil || info.Mode()&os.ModeSymlink != 0 {
			return ErrSourceIdentityChanged
		}
	}
	return nil
}

func statIdentity(info os.FileInfo) string {
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok {
		return ""
	}
	return fmt.Sprintf("%d:%d", uint64(stat.Dev), uint64(stat.Ino))
}

func opaqueSourceID(agent, identity string) string {
	sum := sha256.Sum256([]byte(agent + "\x00" + identity))
	return "src_" + hex.EncodeToString(sum[:8])
}

func defaultExcludedDirs() map[string]struct{} {
	return map[string]struct{}{
		"backup": {}, "backups": {}, "cache": {}, "caches": {},
		"tmp": {}, "temp": {}, ".tmp": {}, ".cache": {},
	}
}
