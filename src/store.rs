//! Event storage abstraction and in-memory implementation.
//!
//! - [`Repository`] — trait for append-only event storage
//! - [`VecRepository`] — naive in-memory store backed by `Vec`
//!
//! Thread safety and snapshot support are deferred.

use crate::event::{SystemEvent, TaskId};
use crate::task::TaskState;

// ---------------------------------------------------------------------------
// Repository trait
// ---------------------------------------------------------------------------

/// An append-only event store.
///
/// Implementations manage the lifecycle of [`Event`](Self::Event) values:
/// persisting, retrieving, and querying them.
///
/// # Design notes
///
/// * `Snapshot` is deliberately absent from this trait to avoid the
///   `dyn Repository` E0191 error (`the value of the associated type
///   Snapshot must be specified`). Snapshot support is deferred.
/// * Thread safety is not required. Implementations that need
///   interior mutability should wrap themselves in a `Mutex` or
///   `RwLock` externally.
pub trait Repository {
    /// The type of event stored in this repository.
    type Event;

    /// Append an event to the store.
    ///
    /// Events are presumed to be ordered by append time. The caller
    /// is responsible for assigning sequencing externally.
    fn append(&mut self, event: Self::Event);

    /// Append multiple events to the store in batch.
    ///
    /// The default implementation delegates to [`append`](Repository::append)
    /// for each item. Implementations may override for efficiency.
    fn append_batch(&mut self, events: Vec<Self::Event>) {
        for event in events {
            self.append(event);
        }
    }

    /// Return all events from position `from` (inclusive) onward.
    ///
    /// Returns an empty [`Vec`] when `from` is beyond the last event.
    ///
    /// # Implementation note
    ///
    /// The default `VecRepository` implementation requires `Event: Clone`
    /// because it clones events eagerly. Other implementations may avoid
    /// this bound by returning references or zero-copy views.
    #[must_use]
    fn stream(&self, from: u64) -> Vec<Self::Event>;

    /// Returns `true` if any event indicates that `task_id` has
    /// failed (i.e. a [`TaskFailed`](crate::event::SystemEvent::TaskFailed)
    /// event exists for that task).
    ///
    /// # Performance
    ///
    /// The default implementation is O(n) in the number of stored events
    /// — it performs a linear scan without indexing. Implementations
    /// backed by indexed storage should override this with an O(log n)
    /// or O(1) lookup.
    #[must_use]
    fn has_failed(&self, task_id: TaskId) -> bool;

    /// The number of events currently stored.
    #[must_use]
    fn len(&self) -> u64;

    /// Returns `true` when no events are stored.
    #[must_use]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---------------------------------------------------------------------------
// VecRepository
// ---------------------------------------------------------------------------

/// A naive in-memory event store backed by a [`Vec`].
///
/// Every call to [`stream`](Repository::stream) clones the requested
/// suffix of the internal vector. This is suitable for testing and
/// development but **not** for production use with large event logs.
///
/// `VecRepository` is `!Sync` (no interior mutability). Wrap in a
/// `Mutex` or `RwLock` for shared access across threads.
///
/// The type parameter `T` is currently constrained to [`SystemEvent`]
/// by the [`Repository`] impl. It remains generic so that the same
/// store type can be reused when `Repository` is implemented for
/// other event types in future releases.
#[derive(Debug, Clone)]
pub struct VecRepository<T> {
    /// The ordered sequence of stored events.
    events: Vec<T>,
}

/// Manual `Default` impl (rather than `#[derive(Default)]`) avoids
/// the implicit `T: Default` bound that the derive would add.
impl<T> Default for VecRepository<T> {
    fn default() -> Self {
        VecRepository { events: Vec::new() }
    }
}

impl Repository for VecRepository<crate::event::SystemEvent> {
    type Event = crate::event::SystemEvent;

    fn append(&mut self, event: crate::event::SystemEvent) {
        self.events.push(event);
    }

    fn append_batch(&mut self, events: Vec<crate::event::SystemEvent>) {
        self.events.extend(events);
    }

    fn stream(&self, from: u64) -> Vec<crate::event::SystemEvent> {
        // `usize::try_from` fails on 32-bit platforms when `from` exceeds
        // `usize::MAX`.  Clamping to `usize::MAX` guarantees an empty result
        // in that case, which matches the "beyond len" contract.
        let from = usize::try_from(from).unwrap_or(usize::MAX);
        self.events.iter().skip(from).cloned().collect()
    }

    fn has_failed(&self, task_id: TaskId) -> bool {
        self.events.iter().any(|event| {
            matches!(
                event,
                crate::event::SystemEvent::TaskFailed { id, .. } if *id == task_id
            )
        })
    }

    fn len(&self) -> u64 {
        self.events.len() as u64
    }
}

// ---------------------------------------------------------------------------
// fold_events
// ---------------------------------------------------------------------------

/// Extract the [`TaskId`] from any event that carries one.
fn event_task_id(event: &SystemEvent) -> Option<TaskId> {
    match event {
        SystemEvent::TaskStarted { id }
        | SystemEvent::TaskDecomposed { id, .. }
        | SystemEvent::TaskStepCompleted { id, .. }
        | SystemEvent::TaskCompleted { id, .. }
        | SystemEvent::TaskFailed { id, .. }
        | SystemEvent::TaskBlocked { id, .. }
        | SystemEvent::ContextCompressed { id, .. }
        | SystemEvent::ArtifactProduced { id, .. }
        | SystemEvent::Evaluation { id, .. }
        | SystemEvent::FeedbackGenerated { id, .. } => Some(*id),
    }
}

/// Reconstruct the last known [`TaskState`] for `task_id` from an event
/// log by replaying lifecycle events in order.
///
/// # Forward-progress guard
///
/// Once the state reaches `Completed` it cannot regress — any subsequent
/// lifecycle events for the same task are ignored. This prevents a
/// stale event (e.g. `TaskStarted`) from rolling back a terminal state.
///
/// # Mapping
///
/// | Event(s) | Resulting state |
/// |----------|----------------|
/// | `TaskStarted` | `Planning` |
/// | `TaskDecomposed` | `Executing` |
/// | `TaskStepCompleted` | `Evaluating` |
/// | `ContextCompressed` | `Feedback` |
/// | `TaskCompleted` / `TaskFailed` / `TaskBlocked` | `Completed` |
/// | All others | No-op |
///
/// # Known gaps
///
/// The `Compressing` intermediate state is never returned because no
/// dedicated [`SystemEvent`] variant exists for entering compression.
/// The `ContextCompressed` event maps directly to `Feedback` (the state
/// after compression). This gap may be addressed in a future release by
/// adding a `TaskCompressing` variant.
///
/// Returns [`None`] when no lifecycle event for `task_id` exists in the
/// log.
#[must_use]
pub fn fold_events(events: &[SystemEvent], task_id: TaskId) -> Option<TaskState> {
    let mut state: Option<TaskState> = None;
    for event in events {
        if event_task_id(event) != Some(task_id) {
            continue;
        }
        // Forward-progress guard: terminal states are final.
        if state == Some(TaskState::Completed) {
            continue;
        }
        match event {
            SystemEvent::TaskStarted { .. } => state = Some(TaskState::Planning),
            SystemEvent::TaskDecomposed { .. } => state = Some(TaskState::Executing),
            SystemEvent::TaskStepCompleted { .. } => state = Some(TaskState::Evaluating),
            SystemEvent::ContextCompressed { .. } => state = Some(TaskState::Feedback),
            SystemEvent::TaskCompleted { .. }
            | SystemEvent::TaskFailed { .. }
            | SystemEvent::TaskBlocked { .. } => state = Some(TaskState::Completed),
            _ => {}
        }
    }
    state
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Error, SystemEvent, TaskId};

    /// Helper to build a [`VecRepository`] seeded with events.
    fn repo_with_events(events: Vec<SystemEvent>) -> VecRepository<SystemEvent> {
        let mut repo: VecRepository<SystemEvent> = VecRepository::default();
        for event in events {
            repo.append(event);
        }
        repo
    }

    // -- Append + stream ----------------------------------------------------

    /// Append an event, then stream from 0 — the appended event must
    /// appear in the output.
    #[test]
    fn append_then_stream_returns_event() {
        let mut repo: VecRepository<SystemEvent> = VecRepository::default();
        repo.append(SystemEvent::TaskStarted { id: TaskId(1) });
        let events = repo.stream(0);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            SystemEvent::TaskStarted { id: TaskId(1) }
        ));
    }

    /// `stream(0)` on an empty repository must return an empty vec.
    #[test]
    fn stream_zero_on_empty_repo_returns_empty() {
        let repo: VecRepository<SystemEvent> = VecRepository::default();
        assert!(repo.stream(0).is_empty());
    }

    /// `stream(from)` with `from` >= `len` must return an empty vec.
    #[test]
    fn stream_beyond_len_returns_empty() {
        let repo = repo_with_events(vec![SystemEvent::TaskStarted { id: TaskId(1) }]);
        assert!(repo.stream(1).is_empty(), "at len");
        assert!(repo.stream(2).is_empty(), "beyond len");
        assert!(repo.stream(u64::MAX).is_empty(), "far beyond len");
    }

    /// `stream(from)` with `from` in the middle returns only the
    /// suffix starting at that position.
    #[test]
    fn stream_from_middle_returns_suffix() {
        let events = vec![
            SystemEvent::TaskStarted { id: TaskId(1) },
            SystemEvent::TaskStepCompleted {
                id: TaskId(1),
                step: 0,
            },
            SystemEvent::TaskCompleted {
                id: TaskId(1),
                summary: "done".into(),
            },
        ];
        let repo = repo_with_events(events);
        let suffix = repo.stream(1);
        assert_eq!(suffix.len(), 2);
        assert!(matches!(suffix[0], SystemEvent::TaskStepCompleted { .. }));
        assert!(matches!(suffix[1], SystemEvent::TaskCompleted { .. }));
    }

    // -- has_failed ---------------------------------------------------------

    /// `has_failed` returns `true` when a [`TaskFailed`] event exists
    /// for the given task ID.
    #[test]
    fn has_failed_true_for_failed_task() {
        let repo = repo_with_events(vec![
            SystemEvent::TaskStarted { id: TaskId(1) },
            SystemEvent::TaskFailed {
                id: TaskId(1),
                error: Error::Transient("oops".into()),
            },
        ]);
        assert!(repo.has_failed(TaskId(1)));
    }

    /// `has_failed` returns `false` when a task only has
    /// [`TaskBlocked`] events (blocked ≠ failed).
    #[test]
    fn has_failed_false_for_blocked_task() {
        let repo = repo_with_events(vec![
            SystemEvent::TaskStarted { id: TaskId(1) },
            SystemEvent::TaskBlocked {
                id: TaskId(1),
                reason: "waiting".into(),
            },
        ]);
        assert!(!repo.has_failed(TaskId(1)));
    }

    /// `has_failed` correctly distinguishes between two tasks when
    /// only one of them has a [`TaskFailed`] event.
    #[test]
    fn has_failed_multi_task_distinction() {
        let repo = repo_with_events(vec![
            SystemEvent::TaskStarted { id: TaskId(1) },
            SystemEvent::TaskFailed {
                id: TaskId(1),
                error: Error::Transient("oops".into()),
            },
            SystemEvent::TaskStarted { id: TaskId(2) },
            SystemEvent::TaskCompleted {
                id: TaskId(2),
                summary: "ok".into(),
            },
        ]);
        assert!(repo.has_failed(TaskId(1)));
        assert!(!repo.has_failed(TaskId(2)));
    }

    /// `has_failed` returns `false` for a task ID with no events.
    #[test]
    fn has_failed_false_for_unknown_task_id() {
        let repo = repo_with_events(vec![SystemEvent::TaskStarted { id: TaskId(1) }]);
        assert!(!repo.has_failed(TaskId(99)));
    }

    // -- Default ------------------------------------------------------------

    /// `VecRepository::default()` constructs an empty repository via
    /// the manual `Default` impl, usable from any module without
    /// additional imports.
    #[test]
    fn default_constructs_empty_repo() {
        let repo: VecRepository<SystemEvent> = VecRepository::default();
        assert!(repo.is_empty());
        assert_eq!(repo.len(), 0);
    }

    // -- len / is_empty -----------------------------------------------------

    /// `len` grows with each append; `is_empty` transitions from
    /// `true` to `false`.
    #[test]
    fn len_grows_with_appends() {
        let mut repo: VecRepository<SystemEvent> = VecRepository::default();
        assert!(repo.is_empty());
        repo.append(SystemEvent::TaskStarted { id: TaskId(1) });
        assert_eq!(repo.len(), 1);
        repo.append(SystemEvent::TaskCompleted {
            id: TaskId(1),
            summary: "done".into(),
        });
        assert_eq!(repo.len(), 2);
        assert!(!repo.is_empty());
    }

    // -- fold_events ---------------------------------------------------------

    /// Single [`TaskBlocked`] event folds to `Completed`.
    #[test]
    fn fold_task_blocked_to_completed() {
        let events = vec![SystemEvent::TaskBlocked {
            id: TaskId(1),
            reason: "waiting".into(),
        }];
        assert_eq!(fold_events(&events, TaskId(1)), Some(TaskState::Completed));
    }

    /// Single [`TaskFailed`] event folds to `Completed`.
    #[test]
    fn fold_task_failed_to_completed() {
        let events = vec![SystemEvent::TaskFailed {
            id: TaskId(1),
            error: Error::Transient("oops".into()),
        }];
        assert_eq!(fold_events(&events, TaskId(1)), Some(TaskState::Completed));
    }

    /// Empty event list produces no state for any task.
    #[test]
    fn fold_empty_events_returns_none() {
        let events: Vec<SystemEvent> = vec![];
        assert_eq!(fold_events(&events, TaskId(1)), None);
    }

    /// Two task IDs interleaved — each ends in the correct state.
    #[test]
    fn fold_interleaved_tasks() {
        let events = vec![
            SystemEvent::TaskStarted { id: TaskId(1) },
            SystemEvent::TaskStarted { id: TaskId(2) },
            SystemEvent::TaskDecomposed {
                id: TaskId(1),
                sub_goals: vec![],
            },
            SystemEvent::TaskCompleted {
                id: TaskId(2),
                summary: "done".into(),
            },
            SystemEvent::TaskStepCompleted {
                id: TaskId(1),
                step: 0,
            },
        ];
        assert_eq!(fold_events(&events, TaskId(1)), Some(TaskState::Evaluating));
        assert_eq!(fold_events(&events, TaskId(2)), Some(TaskState::Completed));
    }

    /// Partial recovery: `TaskStarted` → `TaskCompleted` skips intermediate
    /// states but reaches the correct terminal state.
    #[test]
    fn fold_partial_recovery() {
        let events = vec![
            SystemEvent::TaskStarted { id: TaskId(1) },
            SystemEvent::TaskCompleted {
                id: TaskId(1),
                summary: "done".into(),
            },
        ];
        assert_eq!(fold_events(&events, TaskId(1)), Some(TaskState::Completed));
    }

    /// Compensation chain: sub-task blocked, parent blocked, both fold
    /// to `Completed`.
    #[test]
    fn fold_compensation_chain() {
        let events = vec![
            SystemEvent::TaskStarted { id: TaskId(1) },
            SystemEvent::TaskStarted { id: TaskId(2) },
            SystemEvent::TaskFailed {
                id: TaskId(2),
                error: Error::Fatal("fail".into()),
            },
            SystemEvent::TaskBlocked {
                id: TaskId(1),
                reason: "sub-task 2 failed".into(),
            },
        ];
        assert_eq!(fold_events(&events, TaskId(1)), Some(TaskState::Completed));
        assert_eq!(fold_events(&events, TaskId(2)), Some(TaskState::Completed));
    }

    /// Forward-progress guard: [`TaskCompleted`] followed by
    /// [`TaskStarted`] must still yield `Completed`.
    #[test]
    fn fold_forward_progress_guard() {
        let events = vec![
            SystemEvent::TaskCompleted {
                id: TaskId(1),
                summary: "done".into(),
            },
            SystemEvent::TaskStarted { id: TaskId(1) },
        ];
        assert_eq!(
            fold_events(&events, TaskId(1)),
            Some(TaskState::Completed),
            "Completed state must not regress after TaskStarted",
        );
    }

    /// Forward-progress guard: [`TaskFailed`] followed by
    /// [`TaskStarted`] must still yield `Completed`.
    #[test]
    fn fold_forward_progress_guard_from_failed() {
        let events = vec![
            SystemEvent::TaskFailed {
                id: TaskId(1),
                error: Error::Transient("oops".into()),
            },
            SystemEvent::TaskStarted { id: TaskId(1) },
        ];
        assert_eq!(
            fold_events(&events, TaskId(1)),
            Some(TaskState::Completed),
            "Completed (from TaskFailed) must not regress after TaskStarted",
        );
    }
}
