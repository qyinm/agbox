package scheduler

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"errors"
	"fmt"
	"path/filepath"
	"time"

	"github.com/hippoom/agbox/internal/session"
	"github.com/hippoom/agbox/internal/session/jsonl"
	"github.com/hippoom/agbox/internal/store"
)

const (
	DefaultLeaseTTL = 15 * time.Second
	IdlePoll        = 100 * time.Millisecond
)

type ReconcileOptions struct {
	LivePath       string
	BaselineByPath map[string]int64
	CreateReceipt  bool
	FailFast       bool
	Now            time.Time
}

func (c *Controller) Snapshot(now time.Time) ([]session.Source, error) {
	if now.IsZero() {
		now = time.Now()
	}
	historyWindow, err := c.Store.HistoryWindow()
	if err != nil {
		return nil, err
	}
	var sources []session.Source
	var errs []error
	for _, adapter := range c.Adapters {
		if !session.IsRunnable(adapter) {
			continue
		}
		var observed []session.Source
		if configurable, ok := adapter.(session.ConfigurableDiscovery); ok {
			observed, err = configurable.DiscoverSourcesWithOptions(session.DiscoveryOptions{Agent: adapter.Agent(), Now: now, HistoryWindow: historyWindow})
		} else {
			observed, err = adapter.DiscoverSources()
		}
		if err != nil {
			errs = append(errs, fmt.Errorf("%s discovery: %w", adapter.Agent(), err))
			continue
		}
		sources = append(sources, observed...)
	}
	return sources, errors.Join(errs...)
}

type ReconcileResult struct {
	Receipts []string
	Sources  []session.Source
	Warning  error
}

type Controller struct {
	Store    *store.Store
	OwnerID  string
	LeaseTTL time.Duration
	Adapters []session.Adapter
}

func New(s *store.Store) *Controller {
	return &Controller{Store: s, OwnerID: opaqueID("owner_"), LeaseTTL: DefaultLeaseTTL, Adapters: session.All()}
}

func opaqueID(prefix string) string {
	var b [12]byte
	if _, err := rand.Read(b[:]); err != nil {
		return fmt.Sprintf("%s%d", prefix, time.Now().UnixNano())
	}
	return prefix + hex.EncodeToString(b[:])
}

func (c *Controller) adapter(agent string) (session.Adapter, bool) {
	for _, adapter := range c.Adapters {
		if adapter.Agent() == agent {
			if !session.IsRunnable(adapter) {
				return nil, false
			}
			return adapter, true
		}
	}
	return nil, false
}

// Reconcile performs metadata-only discovery and coalesces observations into
// one durable row per source generation. It never parses transcript content.
func (c *Controller) Reconcile(opts ReconcileOptions) (ReconcileResult, error) {
	if opts.Now.IsZero() {
		opts.Now = time.Now()
	}
	var result ReconcileResult
	var warnings []error
	historyWindow, err := c.Store.HistoryWindow()
	if err != nil {
		return result, err
	}
	for _, adapter := range c.Adapters {
		if !session.IsRunnable(adapter) {
			continue
		}
		var sources []session.Source
		var err error
		if configurable, ok := adapter.(session.ConfigurableDiscovery); ok {
			sources, err = configurable.DiscoverSourcesWithOptions(session.DiscoveryOptions{Agent: adapter.Agent(), Now: opts.Now, HistoryWindow: historyWindow})
		} else {
			sources, err = adapter.DiscoverSources()
		}
		if err != nil {
			wrapped := fmt.Errorf("%s discovery: %w", adapter.Agent(), err)
			if opts.FailFast {
				return result, wrapped
			}
			warnings = append(warnings, wrapped)
			continue
		}
		var durable, compatibility []session.Source
		for _, src := range sources {
			if src.FileIdentity != "" {
				durable = append(durable, src)
			} else {
				compatibility = append(compatibility, src)
			}
		}
		_, rooted := adapter.(session.RootedAdapter)
		if rooted || len(durable) > 0 {
			persisted, listErr := c.Store.ActiveSourceGenerations(adapter.Agent())
			if listErr != nil {
				if opts.FailFast {
					return result, listErr
				}
				warnings = append(warnings, listErr)
			} else {
				previous := make([]session.Source, 0, len(persisted))
				for _, prior := range persisted {
					previous = append(previous, session.Source{Agent: prior.Agent, Path: prior.SourceRef, Project: prior.Project,
						RootPath: prior.RootPath, RootClass: session.RootClass(prior.RootClass), SourceID: prior.SourceID,
						Generation: prior.Generation, FileIdentity: prior.FileIdentity, Size: prior.ObservedSize})
				}
				reconciled := session.ReconcileSources(previous, durable)
				for _, stale := range append(reconciled.Replaced, reconciled.Deleted...) {
					if tombstoneErr := c.Store.TombstoneSourceGeneration(stale.SourceID, stale.Generation, opts.Now); tombstoneErr != nil {
						if opts.FailFast {
							return result, tombstoneErr
						}
						warnings = append(warnings, tombstoneErr)
					}
				}
				durable = reconciled.Current
			}
		}
		sources = append(durable, compatibility...)
		result.Sources = append(result.Sources, sources...)
		for _, src := range sources {
			if src.Generation <= 0 {
				src.Generation = 1
			}
			if src.SourceID == "" {
				// Legacy/custom adapters do not provide a generation identity. A
				// path-derived opaque key keeps those adapters durable and bounded.
				src.SourceID = pathSourceID(src.Agent, src.Path)
			}
			if err := c.Store.UpsertSourceGeneration(store.SourceGeneration{
				SourceID: src.SourceID, Generation: src.Generation, Agent: src.Agent,
				SourceRef: src.Path, Project: src.Project, RootPath: src.RootPath, RootClass: string(src.RootClass),
				FileIdentity: src.FileIdentity, ObservedSize: src.Size, State: store.SourceActive,
				CreatedAt: opts.Now, UpdatedAt: opts.Now,
			}); err != nil {
				if opts.FailFast {
					return result, err
				}
				warnings = append(warnings, err)
				continue
			}

			class := store.WorkActiveCatchup
			if src.RootClass == session.RootArchive {
				class = store.WorkArchive
				if !src.HistoricalEligible {
					continue
				}
			}
			isLive := opts.LivePath != "" && samePath(opts.LivePath, src.Path)
			startupBaseline, hadBaseline := opts.BaselineByPath[src.Path]
			if opts.BaselineByPath != nil && (!hadBaseline || src.Size > startupBaseline) {
				isLive = true
			}
			if isLive {
				class = store.WorkLive
			}
			baseline := src.BaselineOffset
			if hadBaseline {
				baseline = startupBaseline
			}
			if isLive {
				// A watched append is eligible regardless of source age. Existing
				// non-zero checkpoints remain intact; a newly created live file
				// starts at zero rather than being mistaken for old history.
				baseline = 0
			}
			cp, cpErr := c.Store.GetIngestionCheckpoint(src.SourceID, src.Generation)
			if cpErr != nil {
				if opts.FailFast {
					return result, cpErr
				}
				warnings = append(warnings, cpErr)
				continue
			}
			pristineCheckpoint := cp.CommittedOffset == 0 && cp.ParserStateVersion == 0 && len(cp.ParserState) == 0
			if pristineCheckpoint && !isLive && src.RootClass == session.RootActive && !src.HistoricalEligible {
				if err := c.Store.InitializeIngestionCheckpoint(src.SourceID, src.Generation, baseline, jsonl.ContextStateVersion, jsonl.MissingContextState(), opts.Now); err != nil {
					if opts.FailFast {
						return result, err
					}
					warnings = append(warnings, err)
					continue
				}
			}
			if !isLive && !src.HistoricalEligible {
				continue
			}
			receipt := ""
			if opts.CreateReceipt {
				receipt = opaqueID("rcpt_")
				result.Receipts = append(result.Receipts, receipt)
			}
			if _, err := c.Store.EnqueueIngestionWork(store.EnqueueWork{
				SourceID: src.SourceID, Generation: src.Generation, Class: class,
				TargetOffset: src.Size, ReceiptID: receipt, Now: opts.Now,
			}); err != nil {
				if opts.FailFast {
					return result, err
				}
				warnings = append(warnings, err)
			}
		}
	}
	result.Warning = errors.Join(warnings...)
	return result, nil
}

func (c *Controller) ReconcileLiveSource(src session.Source, now time.Time) error {
	if now.IsZero() {
		now = time.Now()
	}
	if src.SourceID == "" || src.Generation <= 0 {
		return store.ErrGenerationMismatch
	}
	if err := c.Store.UpsertSourceGeneration(store.SourceGeneration{
		SourceID: src.SourceID, Generation: src.Generation, Agent: src.Agent, SourceRef: src.Path,
		Project: src.Project, RootPath: src.RootPath, RootClass: string(src.RootClass), FileIdentity: src.FileIdentity,
		ObservedSize: src.Size, State: store.SourceActive, CreatedAt: now, UpdatedAt: now,
	}); err != nil {
		return err
	}
	_, err := c.Store.EnqueueIngestionWork(store.EnqueueWork{SourceID: src.SourceID, Generation: src.Generation,
		Class: store.WorkLive, TargetOffset: src.Size, Now: now})
	return err
}

func pathSourceID(agent, path string) string {
	// The database never exposes this identity; avoid persisting a plaintext
	// path as the scheduler handle while retaining deterministic coalescing.
	return sessionSourceHash(agent + "\x00" + filepath.Clean(path))
}

// sessionSourceHash is deliberately local so adapters retain ownership of
// their stronger device/inode identities.
func sessionSourceHash(value string) string {
	// FNV-style mixing is sufficient for an opaque local queue identifier.
	var h uint64 = 1469598103934665603
	for i := 0; i < len(value); i++ {
		h = (h ^ uint64(value[i])) * 1099511628211
	}
	return fmt.Sprintf("src_legacy_%016x", h)
}

func samePath(a, b string) bool {
	aa, errA := filepath.Abs(a)
	bb, errB := filepath.Abs(b)
	return errA == nil && errB == nil && filepath.Clean(aa) == filepath.Clean(bb)
}

// ProcessOne acquires the cross-process fence and executes at most one bounded
// parser slice. false means no runnable work exists.
func (c *Controller) ProcessOne(ctx context.Context) (bool, int, error) {
	select {
	case <-ctx.Done():
		return false, 0, ctx.Err()
	default:
	}
	now := time.Now()
	lease, err := c.Store.AcquireSchedulerLease(c.OwnerID, now, c.LeaseTTL)
	if errors.Is(err, store.ErrLeaseHeld) {
		return false, 0, nil
	}
	if err != nil {
		return false, 0, err
	}
	if err := c.Store.RecoverStaleRunningWork(c.OwnerID, lease.FencingToken, now); err != nil {
		return false, 0, err
	}
	item, err := c.Store.ClaimNextIngestionWork(c.OwnerID, lease.FencingToken, now)
	if errors.Is(err, sql.ErrNoRows) {
		return false, 0, nil
	}
	if err != nil {
		return false, 0, err
	}
	cp, err := c.Store.GetIngestionCheckpoint(item.Work.SourceID, item.Work.Generation)
	if err != nil {
		_ = c.Store.RequeueClaimedWork(item.Work.SourceID, item.Work.Generation, c.OwnerID, lease.FencingToken, time.Now())
		return true, 0, err
	}
	adapter, ok := c.adapter(item.Source.Agent)
	if !ok {
		err = fmt.Errorf("adapter %q is not runnable", item.Source.Agent)
		_ = c.quarantine(item, cp, lease, "unsupported_adapter")
		return true, 0, err
	}
	src := session.Source{
		Agent: item.Source.Agent, Path: item.Source.SourceRef,
		Project: item.Source.Project, RootPath: item.Source.RootPath, RootClass: session.RootClass(item.Source.RootClass),
		FileIdentity: item.Source.FileIdentity, SourceID: item.Source.SourceID,
		Generation: item.Source.Generation, Size: item.Work.TargetOffset,
	}
	parsed, err := adapter.ParseDelta(src, session.Cursor{
		SourcePath: src.Path, LastOffset: cp.CommittedOffset,
		ParserStateVersion: cp.ParserStateVersion, ParserState: cp.ParserState,
	})
	if err != nil {
		_ = c.quarantine(item, cp, lease, classifyFailure(err))
		return true, 0, err
	}
	if parsed.NewOffset < cp.CommittedOffset || (parsed.NewOffset == cp.CommittedOffset && item.Work.TargetOffset > cp.CommittedOffset && !parsed.Incomplete) {
		err := errors.New("parser made no checkpoint progress")
		_ = c.quarantine(item, cp, lease, "no_progress")
		return true, 0, err
	}
	complete := parsed.NewOffset >= item.Work.TargetOffset
	watermark := time.Now().UnixNano()
	if watermark <= cp.VisibilityWatermark {
		watermark = cp.VisibilityWatermark + 1
	}
	err = c.Store.CommitParsedIngestionSlice(store.SliceCommit{
		SourceID: item.Work.SourceID, Generation: item.Work.Generation,
		ExpectedOffset: cp.CommittedOffset, NextOffset: parsed.NewOffset,
		ParserStateVersion: parsed.ParserStateVersion, ParserState: parsed.ParserState,
		VisibilityWatermark: watermark, LeaseOwner: c.OwnerID,
		FencingToken: lease.FencingToken, Now: time.Now(), Complete: complete, AwaitingAppend: parsed.Incomplete,
	}, store.ParsedSlice{Session: parsed.Session, Turns: parsed.Turns, Actions: parsed.Actions,
		Corrections: parsed.Corrections, CursorHash: parsed.NewHash})
	if err != nil {
		_ = c.Store.RequeueClaimedWork(item.Work.SourceID, item.Work.Generation, c.OwnerID, lease.FencingToken, time.Now())
	}
	return true, len(parsed.Corrections), err
}

func (c *Controller) quarantine(item store.RunnableIngestion, cp store.IngestionCheckpoint, lease store.SchedulerLease, failure string) error {
	return c.Store.QuarantineSource(store.QuarantineRequest{SourceID: item.Work.SourceID, Generation: item.Work.Generation,
		ExpectedOffset: cp.CommittedOffset, FailureClass: failure, LeaseOwner: c.OwnerID,
		FencingToken: lease.FencingToken, Now: time.Now()})
}

func classifyFailure(err error) string {
	switch {
	case errors.Is(err, jsonl.ErrSignalTooLarge):
		return store.FailureSignalTooLarge
	case errors.Is(err, jsonl.ErrRecordBudget):
		return store.FailureRecordBudget
	case errors.Is(err, jsonl.ErrMalformedRecord):
		return store.FailureMalformedRecord
	case errors.Is(err, jsonl.ErrMissingContext):
		return store.FailureMissingContext
	default:
		return store.FailureParse
	}
}

func (c *Controller) Run(ctx context.Context) error {
	ticker := time.NewTicker(IdlePoll)
	defer ticker.Stop()
	for {
		worked, _, err := c.ProcessOne(ctx)
		if err != nil && !errors.Is(err, context.Canceled) {
			// A source failure is already isolated by quarantine. Lease/store
			// failures are retried after a bounded delay.
		}
		if worked {
			continue
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
		}
	}
}

func WaitReceipts(ctx context.Context, s *store.Store, receipts []string) error {
	ticker := time.NewTicker(25 * time.Millisecond)
	defer ticker.Stop()
	pending := make(map[string]struct{}, len(receipts))
	for _, id := range receipts {
		if id != "" {
			pending[id] = struct{}{}
		}
	}
	var failures []error
	for len(pending) > 0 {
		ids := make([]string, 0, len(pending))
		for id := range pending {
			ids = append(ids, id)
		}
		for start := 0; start < len(ids); start += 500 {
			end := min(start+500, len(ids))
			receipts, err := s.GetIngestionReceipts(ids[start:end])
			if err != nil {
				return err
			}
			if len(receipts) != end-start {
				return sql.ErrNoRows
			}
			for _, receipt := range receipts {
				id := receipt.ReceiptID
				switch receipt.Status {
				case store.ReceiptCompleted:
					delete(pending, id)
				case store.ReceiptQuarantined:
					delete(pending, id)
					failures = append(failures, fmt.Errorf("source %s quarantined: %s", receipt.SourceID, receipt.FailureClass))
				}
			}
		}
		if len(pending) == 0 {
			return errors.Join(failures...)
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
		}
	}
	return errors.Join(failures...)
}
