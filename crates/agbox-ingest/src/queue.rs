use std::{
    collections::{HashMap, VecDeque},
    fmt,
};

/// Default maximum number of sources waiting to be ingested.
pub const SOURCE_QUEUE_CAPACITY: usize = 256;
/// Default number of source decoding workers.
pub const DECODER_WORKERS: usize = 4;
/// Largest supported configured source queue capacity.
pub const MAX_SOURCE_QUEUE_CAPACITY: usize = 4_096;
/// Largest supported configured source decoder worker count.
pub const MAX_DECODER_WORKERS: usize = 16;

const PRIORITY_COUNT: usize = 3;

/// Work urgency, ordered from archive replay to new live activity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum WorkPriority {
    /// Historical archive work.
    Archive,
    /// Catch-up work for an active source.
    ActiveCatchup,
    /// Newly observed live source work.
    Live,
}

impl WorkPriority {
    const fn index(self) -> usize {
        match self {
            Self::Archive => 0,
            Self::ActiveCatchup => 1,
            Self::Live => 2,
        }
    }
}

/// A validated source generation identity used to coalesce queued work.
///
/// ```compile_fail
/// use agbox_ingest::SourceKey;
///
/// let _ = SourceKey {
///     source_id: "../../untrusted".to_owned(),
///     generation: 0,
/// };
/// ```
///
/// Source keys can only be created through [`SourceKey::new`].
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct SourceKey {
    /// Canonical source identifier.
    source_id: String,
    /// Monotonically increasing source generation.
    generation: u64,
}

impl SourceKey {
    /// Creates a source key after validating the persisted source identity contract.
    ///
    /// # Errors
    ///
    /// Returns [`SourceKeyError`] when the source ID is not canonical or the
    /// generation cannot be persisted safely.
    pub fn new(source_id: impl Into<String>, generation: u64) -> Result<Self, SourceKeyError> {
        let source_id = source_id.into();
        if !valid_source_id(&source_id) {
            return Err(SourceKeyError::InvalidSourceId);
        }
        if generation == 0 || generation > i64::MAX as u64 {
            return Err(SourceKeyError::InvalidGeneration);
        }
        Ok(Self {
            source_id,
            generation,
        })
    }

    /// Returns the canonical source identifier.
    #[must_use]
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// Returns the validated source generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

impl fmt::Debug for SourceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceKey")
            .field("source_id_bytes", &self.source_id.len())
            .field("generation", &self.generation)
            .finish()
    }
}

/// Source-key validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SourceKeyError {
    /// The source ID is not a canonical `source_` digest.
    #[error("source identifier is invalid")]
    InvalidSourceId,
    /// The source generation is zero or cannot be represented by the store.
    #[error("source generation is invalid")]
    InvalidGeneration,
}

/// A queued source generation and the furthest offset it should process.
#[derive(Clone, Eq, PartialEq)]
pub struct QueueItem {
    /// Source generation to process.
    pub key: SourceKey,
    /// Largest target offset signalled for this source generation.
    pub target_offset: u64,
    /// Current queue urgency.
    pub priority: WorkPriority,
}

impl fmt::Debug for QueueItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QueueItem")
            .field("key", &self.key)
            .field("target_offset", &self.target_offset)
            .field("priority", &self.priority)
            .finish()
    }
}

/// The result of accepting source work into a queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnqueueOutcome {
    /// A new source generation occupied one queue slot.
    Inserted,
    /// An existing source generation was updated in place.
    Coalesced,
}

/// Failure while placing source work into the queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QueueError {
    /// A distinct source generation could not fit in the fixed-size queue.
    #[error("source queue is full (capacity {capacity})")]
    Full {
        /// Fixed number of pending source generations supported by the queue.
        capacity: usize,
    },
}

/// Invalid daemon queue or worker configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QueueConfigError {
    /// Source queue capacity must be between one and its supported maximum.
    #[error("source queue capacity is outside the supported range")]
    InvalidSourceQueueCapacity,
    /// Decoder worker count must be between one and its supported maximum.
    #[error("decoder worker count is outside the supported range")]
    InvalidDecoderWorkers,
}

/// Validates a configured source queue capacity.
///
/// # Errors
///
/// Returns [`QueueConfigError::InvalidSourceQueueCapacity`] when `capacity`
/// is zero or exceeds [`MAX_SOURCE_QUEUE_CAPACITY`].
pub const fn validate_source_queue_capacity(capacity: usize) -> Result<usize, QueueConfigError> {
    if capacity == 0 || capacity > MAX_SOURCE_QUEUE_CAPACITY {
        Err(QueueConfigError::InvalidSourceQueueCapacity)
    } else {
        Ok(capacity)
    }
}

/// Validates a configured decoder worker count.
///
/// # Errors
///
/// Returns [`QueueConfigError::InvalidDecoderWorkers`] when `workers` is zero
/// or exceeds [`MAX_DECODER_WORKERS`].
pub const fn validate_decoder_workers(workers: usize) -> Result<usize, QueueConfigError> {
    if workers == 0 || workers > MAX_DECODER_WORKERS {
        Err(QueueConfigError::InvalidDecoderWorkers)
    } else {
        Ok(workers)
    }
}

/// Fixed-capacity, keyed source work queue with stable priority ordering.
pub struct KeyedQueue {
    capacity: usize,
    pending: HashMap<SourceKey, QueueItem>,
    index: [VecDeque<SourceKey>; PRIORITY_COUNT],
}

impl KeyedQueue {
    /// Creates an empty queue with room for `capacity` distinct source generations.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            pending: HashMap::with_capacity(capacity),
            index: std::array::from_fn(|_| VecDeque::with_capacity(capacity)),
        }
    }

    /// Returns the fixed maximum number of distinct pending source generations.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the number of distinct pending source generations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Returns whether no source generation is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Returns the number of priority-index slots currently held.
    ///
    /// Each pending item has exactly one index slot, so this is always at most
    /// [`Self::capacity`].
    #[must_use]
    pub fn index_len(&self) -> usize {
        self.index.iter().map(VecDeque::len).sum()
    }

    /// Adds or coalesces work for one source generation.
    ///
    /// Coalescing retains the greatest target offset. A higher-priority signal
    /// moves the item to the end of that priority's FIFO lane. The old lane
    /// entry is removed immediately, keeping index memory bounded by capacity.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Full`] only for a new key when no queue slot is
    /// available; existing keys continue to coalesce while full.
    pub fn try_enqueue(
        &mut self,
        key: SourceKey,
        target_offset: u64,
        priority: WorkPriority,
    ) -> Result<EnqueueOutcome, QueueError> {
        if let Some(item) = self.pending.get_mut(&key) {
            item.target_offset = item.target_offset.max(target_offset);
            if priority > item.priority {
                let old_priority = item.priority;
                item.priority = priority;
                self.remove_index_entry(old_priority, &key);
                self.index[priority.index()].push_back(key);
            }
            return Ok(EnqueueOutcome::Coalesced);
        }

        if self.pending.len() == self.capacity {
            return Err(QueueError::Full {
                capacity: self.capacity,
            });
        }

        self.index[priority.index()].push_back(key.clone());
        self.pending.insert(
            key.clone(),
            QueueItem {
                key,
                target_offset,
                priority,
            },
        );
        Ok(EnqueueOutcome::Inserted)
    }

    /// Removes the next item, prioritizing `Live`, then `ActiveCatchup`, then `Archive`.
    pub fn pop(&mut self) -> Option<QueueItem> {
        for priority in [
            WorkPriority::Live,
            WorkPriority::ActiveCatchup,
            WorkPriority::Archive,
        ] {
            while let Some(key) = self.index[priority.index()].pop_front() {
                if self
                    .pending
                    .get(&key)
                    .is_some_and(|item| item.priority == priority)
                {
                    return self.pending.remove(&key);
                }
            }
        }
        None
    }

    fn remove_index_entry(&mut self, priority: WorkPriority, key: &SourceKey) {
        self.index[priority.index()].retain(|candidate| candidate != key);
    }
}

impl fmt::Debug for KeyedQueue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KeyedQueue")
            .field("capacity", &self.capacity)
            .field("len", &self.pending.len())
            .field("index_len", &self.index_len())
            .finish_non_exhaustive()
    }
}

fn valid_source_id(value: &str) -> bool {
    value.len() == 39
        && value.strip_prefix("source_").is_some_and(|digest| {
            digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
}
