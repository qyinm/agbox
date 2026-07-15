// Command agbox-release-gate exercises the ingestion resource contracts in a
// separate process. It generates fixtures at runtime; no transcript-sized test
// data is checked into the repository.
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/scheduler"
	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/session/codex"
	"github.com/hippoom/agbox/internal/session/jsonl"
	"github.com/hippoom/agbox/internal/store"
	"github.com/hippoom/agbox/internal/watcher"
)

const (
	reportVersion      = 1
	idleRSSLimit       = int64(50 << 20)
	processingRSSLimit = int64(200 << 20)
	eofFixtureBytes    = int64(838 << 20)
)

type profileConfig struct {
	Name               string
	IrrelevantBytes    int64
	SourceCount        int
	LogicalCorpusBytes int64
	RecordsPerSecond   int
	LoadDuration       time.Duration
}

func configFor(name string) (profileConfig, bool) {
	switch name {
	case "smoke":
		return profileConfig{Name: name, IrrelevantBytes: 32 << 20, SourceCount: 25,
			LogicalCorpusBytes: 64 << 20, RecordsPerSecond: 50, LoadDuration: time.Second}, true
	case "release":
		return profileConfig{Name: name, IrrelevantBytes: 32 << 20, SourceCount: 2500,
			LogicalCorpusBytes: 5 << 30, RecordsPerSecond: 50, LoadDuration: 60 * time.Second}, true
	default:
		return profileConfig{}, false
	}
}

type report struct {
	Version    int          `json:"version"`
	Profile    string       `json:"profile"`
	Passed     bool         `json:"passed"`
	StartedAt  time.Time    `json:"started_at"`
	DurationMS int64        `json:"duration_ms"`
	Cases      []caseResult `json:"cases"`
}

type caseResult struct {
	Name                string `json:"name"`
	Passed              bool   `json:"passed"`
	ErrorCode           string `json:"error_code,omitempty"`
	PeakRSSBytes        int64  `json:"peak_rss_bytes"`
	RSSLimitBytes       int64  `json:"rss_limit_bytes,omitempty"`
	BytesRead           int64  `json:"bytes_read,omitempty"`
	FixtureBytes        int64  `json:"fixture_bytes,omitempty"`
	SourceCount         int    `json:"source_count,omitempty"`
	LogicalCorpusBytes  int64  `json:"logical_corpus_bytes,omitempty"`
	RecordsPerSecond    int    `json:"records_per_second,omitempty"`
	LoadDurationSeconds int64  `json:"load_duration_seconds,omitempty"`
	VisibleRecords      int    `json:"visible_records,omitempty"`
	P95VisibilityMS     int64  `json:"p95_visibility_ms,omitempty"`
	P99VisibilityMS     int64  `json:"p99_visibility_ms,omitempty"`
	CatchupPreempted    bool   `json:"catchup_preempted,omitempty"`
	CatchupProgressed   bool   `json:"catchup_progressed,omitempty"`
	RuntimeSysBytes     int64  `json:"runtime_sys_bytes,omitempty"`
}

func main() {
	profileName := flag.String("profile", "smoke", "gate profile: smoke or release")
	selectedCase := flag.String("case", "all", "case: all, eof, irrelevant, idle, or load")
	worker := flag.String("worker", "", "internal worker mode")
	fixture := flag.String("fixture", "", "internal fixture directory")
	flag.Parse()

	cfg, ok := configFor(*profileName)
	if !ok {
		fmt.Fprintln(os.Stderr, "profile must be smoke or release")
		os.Exit(2)
	}
	if *worker != "" {
		result := runWorker(*worker, *fixture, cfg)
		_ = json.NewEncoder(os.Stdout).Encode(result)
		return
	}

	result := runParent(cfg, *selectedCase)
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	_ = enc.Encode(result)
	if !result.Passed {
		os.Exit(1)
	}
}

func runParent(cfg profileConfig, selected string) report {
	started := time.Now()
	out := report{Version: reportVersion, Profile: cfg.Name, Passed: true, StartedAt: started.UTC()}
	cases := []string{"eof", "irrelevant", "idle", "load"}
	if selected != "all" {
		if !slices.Contains(cases, selected) {
			out.Passed = false
			out.Cases = []caseResult{{Name: selected, Passed: false, ErrorCode: "unknown_case"}}
			return out
		}
		cases = []string{selected}
	}
	root, err := os.MkdirTemp("", "agbox-release-gate-")
	if err != nil {
		out.Passed = false
		out.Cases = []caseResult{{Name: "setup", Passed: false, ErrorCode: "tempdir_failed"}}
		return out
	}
	defer os.RemoveAll(root)

	executable, err := os.Executable()
	if err != nil {
		out.Passed = false
		out.Cases = []caseResult{{Name: "setup", Passed: false, ErrorCode: "executable_lookup_failed"}}
		return out
	}
	for _, name := range cases {
		caseDir := filepath.Join(root, name)
		if err := os.MkdirAll(caseDir, 0o700); err != nil {
			out.Cases = append(out.Cases, caseResult{Name: name, Passed: false, ErrorCode: "fixture_setup_failed"})
			out.Passed = false
			continue
		}
		if err := prepareFixture(name, caseDir, cfg); err != nil {
			out.Cases = append(out.Cases, caseResult{Name: name, Passed: false, ErrorCode: "fixture_setup_failed"})
			out.Passed = false
			continue
		}
		result := runChild(executable, name, caseDir, cfg)
		out.Cases = append(out.Cases, result)
		out.Passed = out.Passed && result.Passed
	}
	out.DurationMS = time.Since(started).Milliseconds()
	return out
}

func prepareFixture(name, dir string, cfg profileConfig) error {
	switch name {
	case "eof":
		f, err := os.OpenFile(filepath.Join(dir, "source.jsonl"), os.O_CREATE|os.O_RDWR|os.O_TRUNC, 0o600)
		if err != nil {
			return err
		}
		err = f.Truncate(eofFixtureBytes)
		return errors.Join(err, f.Close())
	case "irrelevant":
		f, err := os.OpenFile(filepath.Join(dir, "source.jsonl"), os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o600)
		if err != nil {
			return err
		}
		if _, err = io.WriteString(f, `{"type":"ignored","payload":"`); err == nil {
			err = writeRepeated(f, 'x', cfg.IrrelevantBytes)
		}
		if err == nil {
			_, err = io.WriteString(f, `"}`+"\n")
		}
		return errors.Join(err, f.Close())
	default:
		return nil
	}
}

func writeRepeated(w io.Writer, value byte, count int64) error {
	chunk := bytes.Repeat([]byte{value}, 32<<10)
	for count > 0 {
		n := int64(len(chunk))
		if n > count {
			n = count
		}
		if _, err := w.Write(chunk[:n]); err != nil {
			return err
		}
		count -= n
	}
	return nil
}

func runChild(executable, name, fixture string, cfg profileConfig) caseResult {
	timeout := 20 * time.Second
	if cfg.Name == "release" && name == "load" {
		// The 60-second load window begins only after 2,500 durable source and
		// queue rows are created. Leave setup/teardown headroom while staying
		// inside the workflow's five-minute gate budget.
		timeout = 4 * time.Minute
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, executable, "--profile", cfg.Name, "--worker", name, "--fixture", fixture)
	cmd.Env = append(os.Environ(), "HOME="+filepath.Join(fixture, "home"), "AGBOX_HOME="+filepath.Join(fixture, "agbox-home"))
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Start(); err != nil {
		return caseResult{Name: name, Passed: false, ErrorCode: "worker_start_failed"}
	}
	peak := int64(0)
	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()
	ticker := time.NewTicker(10 * time.Millisecond)
	defer ticker.Stop()
	var waitErr error
	for waitErr == nil {
		if rss, err := processRSSBytes(cmd.Process.Pid); err == nil && rss > peak {
			peak = rss
		}
		select {
		case waitErr = <-done:
			if waitErr == nil {
				waitErr = io.EOF // successful completion sentinel
			}
		case <-ticker.C:
		}
	}
	if ctx.Err() != nil {
		return caseResult{Name: name, Passed: false, ErrorCode: "worker_timeout", PeakRSSBytes: peak}
	}
	if !errors.Is(waitErr, io.EOF) {
		return caseResult{Name: name, Passed: false, ErrorCode: "worker_failed", PeakRSSBytes: peak}
	}
	var result caseResult
	if err := json.Unmarshal(stdout.Bytes(), &result); err != nil {
		return caseResult{Name: name, Passed: false, ErrorCode: "worker_output_invalid", PeakRSSBytes: peak}
	}
	if peak == 0 {
		peak = result.RuntimeSysBytes
	}
	result.PeakRSSBytes = peak
	result.Passed = result.Passed && peak > 0 && (result.RSSLimitBytes == 0 || peak <= result.RSSLimitBytes)
	if peak == 0 && result.ErrorCode == "" {
		result.ErrorCode = "rss_unavailable"
	} else if result.RSSLimitBytes > 0 && peak > result.RSSLimitBytes && result.ErrorCode == "" {
		result.ErrorCode = "rss_limit_exceeded"
	}
	return result
}

func processRSSBytes(pid int) (int64, error) {
	if runtime.GOOS == "linux" {
		data, err := os.ReadFile(filepath.Join("/proc", strconv.Itoa(pid), "status"))
		if err != nil {
			return 0, err
		}
		for _, line := range strings.Split(string(data), "\n") {
			if strings.HasPrefix(line, "VmRSS:") {
				fields := strings.Fields(line)
				if len(fields) >= 2 {
					kb, err := strconv.ParseInt(fields[1], 10, 64)
					return kb << 10, err
				}
			}
		}
		return 0, errors.New("rss missing")
	}
	out, err := exec.Command("ps", "-o", "rss=", "-p", strconv.Itoa(pid)).Output()
	if err != nil {
		return 0, err
	}
	kb, err := strconv.ParseInt(strings.TrimSpace(string(out)), 10, 64)
	return kb << 10, err
}

func runWorker(name, fixture string, cfg profileConfig) caseResult {
	var result caseResult
	switch name {
	case "eof":
		result = eofWorker(fixture)
	case "irrelevant":
		result = irrelevantWorker(fixture)
	case "idle":
		result = idleWorker(fixture)
	case "load":
		result = loadWorker(fixture, cfg)
	default:
		result = caseResult{Name: name, Passed: false, ErrorCode: "unknown_worker"}
	}
	var stats runtime.MemStats
	runtime.ReadMemStats(&stats)
	result.RuntimeSysBytes = int64(stats.Sys)
	// Keep short-lived workers observable long enough for the parent RSS sampler.
	time.Sleep(150 * time.Millisecond)
	return result
}

func eofWorker(dir string) caseResult {
	path := filepath.Join(dir, "source.jsonl")
	info, err := os.Stat(path)
	if err != nil {
		return caseResult{Name: "eof", Passed: false, ErrorCode: "fixture_stat_failed", RSSLimitBytes: processingRSSLimit}
	}
	result, err := codex.New().ParseDelta(session.Source{Agent: "codex", Path: path, Project: "gate", SourceID: "src_eof", Generation: 1},
		session.Cursor{LastOffset: info.Size(), ParserStateVersion: jsonl.ContextStateVersion, ParserState: []byte(`{"t":0}`)})
	passed := err == nil && result.NewOffset == info.Size() && result.BytesRead == 0
	errorCode := ""
	if !passed {
		errorCode = "eof_read_contract_failed"
	}
	return caseResult{Name: "eof", Passed: passed, ErrorCode: errorCode, RSSLimitBytes: processingRSSLimit,
		BytesRead: result.BytesRead, FixtureBytes: info.Size()}
}

func irrelevantWorker(dir string) caseResult {
	path := filepath.Join(dir, "source.jsonl")
	info, err := os.Stat(path)
	if err != nil {
		return caseResult{Name: "irrelevant", Passed: false, ErrorCode: "fixture_stat_failed", RSSLimitBytes: processingRSSLimit}
	}
	result, err := codex.New().ParseDelta(session.Source{Agent: "codex", Path: path, Project: "gate", SourceID: "src_irrelevant", Generation: 1}, session.Cursor{})
	passed := err == nil && result.NewOffset == info.Size() && result.BytesRead == info.Size() && len(result.Corrections) == 0
	errorCode := ""
	if !passed {
		errorCode = "irrelevant_record_contract_failed"
	}
	return caseResult{Name: "irrelevant", Passed: passed, ErrorCode: errorCode, RSSLimitBytes: processingRSSLimit,
		BytesRead: result.BytesRead, FixtureBytes: info.Size()}
}

func idleWorker(dir string) caseResult {
	home := filepath.Join(dir, "home")
	for _, path := range []string{
		filepath.Join(home, ".claude", "projects"), filepath.Join(home, ".codex", "sessions"),
		filepath.Join(home, ".codex", "archived_sessions"),
	} {
		if err := os.MkdirAll(path, 0o700); err != nil {
			return caseResult{Name: "idle", Passed: false, ErrorCode: "fixture_setup_failed", RSSLimitBytes: idleRSSLimit}
		}
	}
	s, err := store.Open(filepath.Join(dir, "idle.db"))
	if err != nil {
		return caseResult{Name: "idle", Passed: false, ErrorCode: "store_open_failed", RSSLimitBytes: idleRSSLimit}
	}
	defer s.Close()
	ctx, cancel := context.WithCancel(context.Background())
	ready := make(chan struct{})
	done := make(chan error, 1)
	go func() { done <- watcher.RunWithReady(ctx, s, time.Hour, ready) }()
	select {
	case <-ready:
	case <-time.After(5 * time.Second):
		cancel()
		return caseResult{Name: "idle", Passed: false, ErrorCode: "watcher_ready_timeout", RSSLimitBytes: idleRSSLimit}
	}
	time.Sleep(750 * time.Millisecond)
	cancel()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		return caseResult{Name: "idle", Passed: false, ErrorCode: "watcher_stop_timeout", RSSLimitBytes: idleRSSLimit}
	}
	return caseResult{Name: "idle", Passed: true, RSSLimitBytes: idleRSSLimit}
}

func loadWorker(dir string, cfg profileConfig) caseResult {
	result := caseResult{Name: "load", RSSLimitBytes: processingRSSLimit, SourceCount: cfg.SourceCount,
		LogicalCorpusBytes: cfg.LogicalCorpusBytes, RecordsPerSecond: cfg.RecordsPerSecond,
		LoadDurationSeconds: int64(cfg.LoadDuration / time.Second)}
	s, err := store.Open(filepath.Join(dir, "load.db"))
	if err != nil {
		result.ErrorCode = "store_open_failed"
		return result
	}
	defer s.Close()
	now := time.Now()
	catchupCount := cfg.SourceCount - 1
	perSource := cfg.LogicalCorpusBytes / int64(max(1, catchupCount))
	remainder := cfg.LogicalCorpusBytes - perSource*int64(catchupCount)
	for i := 0; i < catchupCount; i++ {
		targetSize := perSource
		if i == catchupCount-1 {
			targetSize += remainder
		}
		path := filepath.Join(dir, fmt.Sprintf("catchup-%05d.jsonl", i))
		f, createErr := os.OpenFile(path, os.O_CREATE|os.O_RDWR|os.O_TRUNC, 0o600)
		if createErr == nil && i == 0 {
			_, createErr = io.WriteString(f, `{"type":"ignored","payload":"`)
			payloadBytes := targetSize - int64(len(`{"type":"ignored","payload":""}`+"\n"))
			if createErr == nil && payloadBytes > 0 {
				createErr = writeRepeated(f, 'x', payloadBytes)
			}
			if createErr == nil {
				_, createErr = io.WriteString(f, `"}`+"\n")
			}
		} else if createErr == nil {
			createErr = f.Truncate(targetSize)
		}
		if createErr != nil || f.Close() != nil {
			result.ErrorCode = "catchup_fixture_failed"
			return result
		}
		id := fmt.Sprintf("src_catchup_%05d", i)
		if err := s.UpsertSourceGeneration(store.SourceGeneration{SourceID: id, Generation: 1, Agent: "codex", SourceRef: path, State: store.SourceActive, CreatedAt: now, UpdatedAt: now}); err != nil {
			result.ErrorCode = "catchup_source_failed"
			return result
		}
		if _, err := s.EnqueueIngestionWork(store.EnqueueWork{SourceID: id, Generation: 1, Class: store.WorkActiveCatchup, TargetOffset: targetSize, Now: now}); err != nil {
			result.ErrorCode = "catchup_enqueue_failed"
			return result
		}
	}
	livePath := filepath.Join(dir, "live.jsonl")
	action := fmt.Sprintf(`{"timestamp":%q,"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"echo gate"}}`, now.UTC().Format(time.RFC3339Nano)) + "\n"
	if err := os.WriteFile(livePath, []byte(action), 0o600); err != nil {
		result.ErrorCode = "live_fixture_failed"
		return result
	}
	const liveID = "src_live_gate"
	if err := s.UpsertSourceGeneration(store.SourceGeneration{SourceID: liveID, Generation: 1, Agent: "codex", SourceRef: livePath, State: store.SourceActive, CreatedAt: now, UpdatedAt: now}); err != nil {
		result.ErrorCode = "live_source_failed"
		return result
	}
	info, _ := os.Stat(livePath)
	if _, err := s.EnqueueIngestionWork(store.EnqueueWork{SourceID: liveID, Generation: 1, Class: store.WorkLive, TargetOffset: info.Size(), Now: now}); err != nil {
		result.ErrorCode = "live_enqueue_failed"
		return result
	}
	controller := scheduler.New(s)
	controller.Adapters = []session.Adapter{codex.New()}
	workerCtx, stopWorker := context.WithCancel(context.Background())
	workerDone := make(chan error, 1)
	go func() { workerDone <- controller.Run(workerCtx) }()
	defer func() {
		stopWorker()
		<-workerDone
	}()
	if err := waitFor(5*time.Second, func() (bool, error) {
		work, err := s.GetIngestionWork(liveID, 1)
		return err == nil && work.State == store.WorkComplete, err
	}); err != nil {
		result.ErrorCode = "initial_live_process_failed"
		return result
	}

	records := cfg.RecordsPerSecond * int(cfg.LoadDuration/time.Second)
	latencies := make([]time.Duration, 0, records)
	appendedAt := make([]time.Time, 0, records)
	interval := time.Second / time.Duration(cfg.RecordsPerSecond)
	loadStarted := time.Now()
	nextAppend := loadStarted
	tailDeadline := loadStarted.Add(cfg.LoadDuration + 5*time.Second)
	liveSource := session.Source{Agent: "codex", Path: livePath, Project: "gate", SourceID: liveID, Generation: 1, RootClass: session.RootActive}
	written, visible := 0, 0
	for visible < records {
		observedAt := time.Now()
		// The producer is clock-driven, not visibility-driven: it continues at
		// 50 records/s while the scheduler independently drains coalesced work.
		for written < records && !observedAt.Before(nextAppend) {
			started := time.Now()
			record := fmt.Sprintf(`{"timestamp":%q,"type":"event_msg","payload":{"type":"user_message","message":"use bounded gate %d"}}`,
				started.UTC().Format(time.RFC3339Nano), written) + "\n"
			f, err := os.OpenFile(livePath, os.O_APPEND|os.O_WRONLY, 0o600)
			if err != nil {
				result.ErrorCode = "live_append_failed"
				return result
			}
			_, writeErr := io.WriteString(f, record)
			closeErr := f.Close()
			if writeErr != nil || closeErr != nil {
				result.ErrorCode = "live_append_failed"
				return result
			}
			info, statErr := os.Stat(livePath)
			if statErr != nil {
				result.ErrorCode = "live_stat_failed"
				return result
			}
			liveSource.Size, liveSource.ModTime = info.Size(), info.ModTime()
			if err := controller.ReconcileLiveSource(liveSource, started); err != nil {
				result.ErrorCode = "live_enqueue_" + gateErrorCode(err)
				return result
			}
			appendedAt = append(appendedAt, started)
			written++
			nextAppend = nextAppend.Add(interval)
			observedAt = time.Now()
		}

		count, countErr := s.CountCorrections()
		if countErr != nil {
			result.ErrorCode = "consumer_visibility_failed"
			return result
		}
		if count > visible {
			observedAt = time.Now()
			upper := min(count, written)
			for visible < upper {
				latencies = append(latencies, observedAt.Sub(appendedAt[visible]))
				visible++
			}
			if !result.CatchupPreempted && s.IngestionHealth().CatchupQueueDepth > 0 {
				result.CatchupPreempted = true
			}
		}
		if written == records && time.Now().After(tailDeadline) {
			result.ErrorCode = "consumer_visibility_failed"
			return result
		}
		time.Sleep(2 * time.Millisecond)
	}
	result.VisibleRecords = visible
	result.P95VisibilityMS = percentile(latencies, 0.95).Milliseconds()
	result.P99VisibilityMS = percentile(latencies, 0.99).Milliseconds()
	if catchupCount > 0 {
		if cp, err := s.GetIngestionCheckpoint("src_catchup_00000", 1); err == nil && cp.CommittedOffset > 0 {
			result.CatchupProgressed = true
		}
	}
	result.Passed = result.CatchupPreempted && result.CatchupProgressed && result.VisibleRecords == records && result.P95VisibilityMS <= 2000 && result.P99VisibilityMS <= 5000
	if !result.Passed {
		result.ErrorCode = "visibility_slo_failed"
	}
	return result
}

func gateErrorCode(err error) string {
	switch {
	case errors.Is(err, store.ErrGenerationMismatch):
		return "generation_mismatch"
	case errors.Is(err, store.ErrStateConflict):
		return "state_conflict"
	case strings.Contains(strings.ToLower(err.Error()), "locked"):
		return "database_locked"
	case strings.Contains(strings.ToLower(err.Error()), "busy"):
		return "database_busy"
	default:
		return "failed"
	}
}

func waitFor(timeout time.Duration, check func() (bool, error)) error {
	deadline := time.Now().Add(timeout)
	for {
		done, err := check()
		if err != nil || done {
			return err
		}
		if time.Now().After(deadline) {
			return context.DeadlineExceeded
		}
		time.Sleep(2 * time.Millisecond)
	}
}

func percentile(values []time.Duration, q float64) time.Duration {
	if len(values) == 0 {
		return 0
	}
	sorted := append([]time.Duration(nil), values...)
	slices.Sort(sorted)
	index := int(float64(len(sorted))*q+0.999999) - 1
	if index < 0 {
		index = 0
	}
	if index >= len(sorted) {
		index = len(sorted) - 1
	}
	return sorted[index]
}
