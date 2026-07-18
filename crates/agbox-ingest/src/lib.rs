mod discovery;
mod history;
mod identity;
mod project;
mod queue;
mod record;

pub use discovery::{
    DISCOVERY_ENTRIES_PER_YIELD, DiscoveryBatch, DiscoveryCursor, DiscoveryError, DiscoveryFault,
    DiscoveryFaultClass, DiscoveryWalker, MAX_DISCOVERY_CURSOR_BYTES,
    deduplicate_overlapping_sources,
};
pub use history::{HISTORY_DAYS, HistoryDecision, HistoryPolicy};
pub use identity::{
    GenerationError, SourceGeneration, SourceSnapshot, VerifiedOpenError, VerifiedSourceOpener,
    reconcile_generation,
};
pub use project::{ProjectError, ProjectResolver, ResolvedProject};
pub use queue::{
    DECODER_WORKERS, EnqueueOutcome, KeyedQueue, MAX_DECODER_WORKERS, MAX_SOURCE_QUEUE_CAPACITY,
    QueueConfigError, QueueError, QueueItem, SOURCE_QUEUE_CAPACITY, SourceKey, SourceKeyError,
    WorkPriority, validate_decoder_workers, validate_source_queue_capacity,
};
pub use record::{READ_BUFFER_BYTES, RecordScanner, RecordWindow, ScanOutcome, WindowReader};
