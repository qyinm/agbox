#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use agbox_ingest::{
    DECODER_WORKERS, EnqueueOutcome, KeyedQueue, MAX_DECODER_WORKERS, MAX_SOURCE_QUEUE_CAPACITY,
    QueueConfigError, QueueError, SOURCE_QUEUE_CAPACITY, SourceKey, SourceKeyError, WorkPriority,
    validate_decoder_workers, validate_source_queue_capacity,
};
use proptest::prelude::*;

const CAPACITY: usize = 8;

fn key(index: u8) -> SourceKey {
    SourceKey::new(format!("source_{index:032x}"), 1).unwrap()
}

#[test]
fn keyed_queue_repeated_signals_coalesce_to_the_largest_offset() {
    let mut queue = KeyedQueue::new(2);
    let key = key(1);
    assert_eq!(
        queue
            .try_enqueue(key.clone(), 20, WorkPriority::Archive)
            .unwrap(),
        EnqueueOutcome::Inserted
    );
    assert_eq!(
        queue
            .try_enqueue(key.clone(), 80, WorkPriority::Live)
            .unwrap(),
        EnqueueOutcome::Coalesced
    );
    let item = queue.pop().unwrap();
    assert_eq!(item.target_offset, 80);
    assert_eq!(item.priority, WorkPriority::Live);
}

#[test]
fn keyed_queue_live_work_preempts_catchup_and_capacity_is_explicit() {
    let mut queue = KeyedQueue::new(2);
    queue.try_enqueue(key(1), 1, WorkPriority::Archive).unwrap();
    queue.try_enqueue(key(2), 1, WorkPriority::Live).unwrap();
    assert_eq!(
        queue.pop().unwrap().key.source_id,
        "source_00000000000000000000000000000002"
    );
    queue
        .try_enqueue(key(3), 1, WorkPriority::ActiveCatchup)
        .unwrap();
    assert_eq!(
        queue.try_enqueue(key(4), 1, WorkPriority::Live),
        Err(QueueError::Full { capacity: 2 })
    );
}

#[test]
fn keyed_queue_promotions_do_not_accumulate_priority_index_entries() {
    let mut queue = KeyedQueue::new(3);
    let key = key(1);
    for _ in 0..1_000 {
        queue
            .try_enqueue(key.clone(), 1, WorkPriority::Archive)
            .unwrap();
        queue
            .try_enqueue(key.clone(), 2, WorkPriority::Live)
            .unwrap();
        assert!(queue.index_len() <= queue.capacity());
    }
    assert_eq!(queue.len(), 1);
    assert_eq!(queue.index_len(), 1);
}

#[test]
fn keyed_queue_fifo_is_stable_within_priority_and_promoted_work_is_not_stale() {
    let mut queue = KeyedQueue::new(4);
    queue.try_enqueue(key(1), 1, WorkPriority::Archive).unwrap();
    queue.try_enqueue(key(2), 1, WorkPriority::Archive).unwrap();
    queue.try_enqueue(key(1), 2, WorkPriority::Live).unwrap();
    queue.try_enqueue(key(3), 1, WorkPriority::Live).unwrap();

    let popped: Vec<_> = std::iter::from_fn(|| queue.pop())
        .map(|item| (item.key.source_id, item.target_offset, item.priority))
        .collect();
    assert_eq!(
        popped,
        vec![
            (
                "source_00000000000000000000000000000001".to_owned(),
                2,
                WorkPriority::Live,
            ),
            (
                "source_00000000000000000000000000000003".to_owned(),
                1,
                WorkPriority::Live,
            ),
            (
                "source_00000000000000000000000000000002".to_owned(),
                1,
                WorkPriority::Archive,
            ),
        ]
    );
}

#[test]
fn keyed_queue_source_keys_and_runtime_limits_use_safe_canonical_contracts() {
    assert_eq!(SOURCE_QUEUE_CAPACITY, 256);
    assert_eq!(DECODER_WORKERS, 4);
    assert_eq!(
        validate_source_queue_capacity(SOURCE_QUEUE_CAPACITY),
        Ok(256)
    );
    assert_eq!(validate_decoder_workers(DECODER_WORKERS), Ok(4));
    assert_eq!(
        validate_source_queue_capacity(MAX_SOURCE_QUEUE_CAPACITY + 1),
        Err(QueueConfigError::InvalidSourceQueueCapacity)
    );
    assert_eq!(
        validate_decoder_workers(MAX_DECODER_WORKERS + 1),
        Err(QueueConfigError::InvalidDecoderWorkers)
    );

    let error = SourceKey::new("../../attacker-controlled-path", 1).unwrap_err();
    assert_eq!(error, SourceKeyError::InvalidSourceId);
    assert!(!format!("{error:?}").contains("attacker-controlled"));
    assert_eq!(
        SourceKey::new("source_00000000000000000000000000000001", 0),
        Err(SourceKeyError::InvalidGeneration)
    );
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelItem {
    target_offset: u64,
    priority: WorkPriority,
    order: u64,
}

#[derive(Debug)]
struct ModelQueue {
    capacity: usize,
    next_order: u64,
    pending: HashMap<SourceKey, ModelItem>,
}

impl ModelQueue {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            next_order: 0,
            pending: HashMap::new(),
        }
    }

    fn enqueue(
        &mut self,
        key: SourceKey,
        target_offset: u64,
        priority: WorkPriority,
    ) -> Result<EnqueueOutcome, QueueError> {
        if let Some(item) = self.pending.get_mut(&key) {
            item.target_offset = item.target_offset.max(target_offset);
            if priority > item.priority {
                item.priority = priority;
                item.order = self.next_order;
                self.next_order += 1;
            }
            return Ok(EnqueueOutcome::Coalesced);
        }
        if self.pending.len() == self.capacity {
            return Err(QueueError::Full {
                capacity: self.capacity,
            });
        }
        self.pending.insert(
            key,
            ModelItem {
                target_offset,
                priority,
                order: self.next_order,
            },
        );
        self.next_order += 1;
        Ok(EnqueueOutcome::Inserted)
    }

    fn pop(&mut self) -> Option<(SourceKey, ModelItem)> {
        let key = self
            .pending
            .iter()
            .min_by_key(|(_, item)| (std::cmp::Reverse(item.priority), item.order))
            .map(|(key, _)| key.clone())?;
        self.pending.remove_entry(&key)
    }
}

proptest! {
    #[test]
    fn keyed_queue_randomized_operations_match_reference_model(
        operations in prop::collection::vec((0u8..12, 0u64..10_000, 0u8..4, any::<bool>()), 10_000)
    ) {
        let mut queue = KeyedQueue::new(CAPACITY);
        let mut model = ModelQueue::new(CAPACITY);

        for (source, offset, priority, pop) in operations {
            if pop {
                let actual = queue.pop().map(|item| (item.key, item.target_offset, item.priority));
                let expected = model.pop().map(|(key, item)| (key, item.target_offset, item.priority));
                prop_assert_eq!(actual, expected);
            } else {
                let priority = match priority % 3 {
                    0 => WorkPriority::Archive,
                    1 => WorkPriority::ActiveCatchup,
                    _ => WorkPriority::Live,
                };
                let key = key(source);
                prop_assert_eq!(
                    queue.try_enqueue(key.clone(), offset, priority),
                    model.enqueue(key, offset, priority),
                );
            }
            prop_assert_eq!(queue.len(), model.pending.len());
            prop_assert!(queue.len() <= queue.capacity());
            prop_assert!(queue.index_len() <= queue.capacity());
        }
    }
}
