package scheduler

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
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
	LivePath      string
	CreateReceipt bool
	FailFast      bool
	Now           time.Time
}

type ReconcileResult struct {
	Receipts []string
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
			if rooted, ok := adapter.(session.RootedAdapter); ok && !rooted.Runnable() {
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
	for _, adapter := range c.Adapters {
		if rooted, ok := adapter.(session.RootedAdapter); ok && !rooted.Runnable() {
			continue
		}
		sources, err := adapter.DiscoverSources()
		if err != nil {
			wrapped := fmt.Errorf("%s discovery: %w", adapter.Agent(), err)
			if opts.FailFast {
				return result, wrapped
			}
			warnings = append(warnings, wrapped)
			continue
		}
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
				SourceRef: src.Path, State: store.SourceActive, CreatedAt: opts.Now, UpdatedAt: opts.Now,
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
			if isLive {
				class = store.WorkLive
			}
			baseline := src.BaselineOffset
			// Some agents do not encode a trusted timestamp in the path. A
			// recent active-file mtime is the bounded fallback for 90-day catchup.
			if src.RootClass == session.RootActive && !src.ModTime.IsZero() && !src.ModTime.Before(opts.Now.Add(-session.DefaultHistoryWindow)) {
				baseline = 0
			}
			if isLive {
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
			if cp.CommittedOffset == 0 && baseline > 0 {
				if err := c.Store.InitializeIngestionCheckpoint(src.SourceID, src.Generation, baseline, jsonl.ContextStateVersion, jsonl.MissingContextState(), opts.Now); err != nil {
					if opts.FailFast {
						return result, err
					}
					warnings = append(warnings, err)
					continue
				}
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
		return true, 0, err
	}
	adapter, ok := c.adapter(item.Source.Agent)
	if !ok {
		err = fmt.Errorf("adapter %q is not runnable", item.Source.Agent)
		_ = c.quarantine(item, cp, lease, "unsupported_adapter")
		return true, 0, err
	}
	info, err := os.Stat(item.Source.SourceRef)
	if err != nil {
		_ = c.quarantine(item, cp, lease, "source_unavailable")
		return true, 0, err
	}
	src := session.Source{
		Agent: item.Source.Agent, Path: item.Source.SourceRef,
		Project: filepath.Base(filepath.Dir(item.Source.SourceRef)), SourceID: item.Source.SourceID,
		Generation: item.Source.Generation, Size: info.Size(), ModTime: info.ModTime(),
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
	complete := parsed.NewOffset >= item.Work.TargetOffset || parsed.Incomplete
	watermark := time.Now().UnixNano()
	if watermark <= cp.VisibilityWatermark {
		watermark = cp.VisibilityWatermark + 1
	}
	err = c.Store.CommitParsedIngestionSlice(store.SliceCommit{
		SourceID: item.Work.SourceID, Generation: item.Work.Generation,
		ExpectedOffset: cp.CommittedOffset, NextOffset: parsed.NewOffset,
		ParserStateVersion: parsed.ParserStateVersion, ParserState: parsed.ParserState,
		VisibilityWatermark: watermark, LeaseOwner: c.OwnerID,
		FencingToken: lease.FencingToken, Now: time.Now(), Complete: complete,
	}, store.ParsedSlice{Session: parsed.Session, Turns: parsed.Turns, Actions: parsed.Actions,
		Corrections: parsed.Corrections, CursorHash: parsed.NewHash})
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
		return "signal_too_large"
	case errors.Is(err, jsonl.ErrRecordBudget):
		return "record_budget"
	case errors.Is(err, jsonl.ErrMalformedRecord):
		return "malformed_record"
	case errors.Is(err, jsonl.ErrMissingContext):
		return "missing_context"
	default:
		return "parse_error"
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
	for len(pending) > 0 {
		for id := range pending {
			receipt, err := s.GetIngestionReceipt(id)
			if err != nil {
				return err
			}
			switch receipt.Status {
			case store.ReceiptCompleted:
				delete(pending, id)
			case store.ReceiptQuarantined:
				return fmt.Errorf("source %s quarantined: %s", receipt.SourceID, receipt.FailureClass)
			}
		}
		if len(pending) == 0 {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
		}
	}
	return nil
}
