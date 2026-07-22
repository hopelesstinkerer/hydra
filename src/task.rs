//! Task types, state machine, and execution loop.
//!
//! The core of hydra is a recursive task execution loop that drives
//! each task through seven states (Idle → Planning → Executing →
//! Evaluating → Compressing → Feedback → Completed). Each state
//! delegates to its corresponding architectural layer.
//!
//! - [`TaskState`] — the state of a task in the lifecycle
//! - [`TaskCompletion`] — how a task finished
//! - [`EvaluationResult`] — the outcome of an evaluation
//! - [`Task`] — the full task record
//! - [`execute_task`] — the state-machine entry point

use std::time::Duration;

use crate::core::Layer;
use crate::event::{Error, SystemEvent, TaskId};
use crate::store::Repository;

// ---------------------------------------------------------------------------
// TaskConfig
// ---------------------------------------------------------------------------

/// Configuration parameters for [`execute_task`].
///
/// Bundles the goal string, recursion depth, and retry backoff delay
/// into a single argument so the function signature remains manageable.
#[derive(Debug, Clone)]
pub struct TaskConfig<'a> {
    /// The goal to execute.
    pub goal: &'a str,
    /// Current nesting depth (starts at 0).
    pub depth: u32,
    /// Delay between retry attempts (pass [`Duration::ZERO`] for tests).
    pub delay: Duration,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of retry attempts for a failing task.
pub const MAX_RETRIES: u32 = 3;

/// Maximum nesting depth for sub-tasks.
pub const MAX_DEPTH: u32 = 100;

// ---------------------------------------------------------------------------
// TaskState
// ---------------------------------------------------------------------------

/// The lifecycle state of a task.
///
/// Every task transitions through these states in order, driven by
/// the [`execute_task`] state machine.
///
/// This enum is `#[non_exhaustive]`; new states may be added in
/// future releases without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TaskState {
    /// Task has been created but not yet started.
    Idle,
    /// The planner is decomposing the goal into sub-goals.
    Planning,
    /// The task is being executed (or sub-tasks are being run).
    Executing,
    /// The task's output is being evaluated.
    Evaluating,
    /// The context window is being compressed.
    Compressing,
    /// Feedback is being generated for self-improvement.
    Feedback,
    /// The task has reached a terminal state.
    Completed,
}

// ---------------------------------------------------------------------------
// TaskCompletion
// ---------------------------------------------------------------------------

/// How a task finished.
///
/// This enum is `#[non_exhaustive]`; new completion modes may be
/// added in future releases without a breaking change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TaskCompletion {
    /// The task completed successfully.
    Success,
    /// The task failed with a human-readable reason.
    Failure(String),
}

// ---------------------------------------------------------------------------
// EvaluationResult
// ---------------------------------------------------------------------------

/// The outcome of evaluating a task's output.
///
/// This enum is `#[non_exhaustive]`; new evaluation outcomes may be
/// added in future releases without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvaluationResult {
    /// The output meets the quality threshold.
    Acceptable,
    /// The output does not meet the quality threshold.
    Unacceptable,
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

/// A full task record capturing the goal, state, and outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Unique identifier for this task.
    pub id: TaskId,
    /// Parent task ID, if this is a sub-task.
    pub parent: Option<TaskId>,
    /// The original goal string.
    pub goal: String,
    /// Current lifecycle state.
    pub state: TaskState,
    /// Number of retry attempts so far.
    pub retry_count: u32,
    /// How the task completed, if terminal.
    pub completion: Option<TaskCompletion>,
    /// Sub-tasks spawned by this task.
    pub sub_tasks: Vec<TaskId>,
}

// ---------------------------------------------------------------------------
// Helper: evaluate
// ---------------------------------------------------------------------------

/// Evaluate a task's output.
///
/// v1 always returns [`EvaluationResult::Acceptable`]. Future versions
/// will implement actual quality metrics. The [`EvaluationResult::Unacceptable`] path in
/// the state machine is wired to produce `TaskFailure` and is ready for
/// use once `evaluate()` returns [`EvaluationResult::Unacceptable`] under a quality-threshold check.
///
/// The `_task` parameter is reserved for future evaluation metrics and
/// is currently unused. It exists so the signature does not change when
/// metrics are implemented.
#[must_use]
pub fn evaluate(_task: &Task) -> EvaluationResult {
    #[cfg(test)]
    if let Some(result) = TEST_EVAL_OVERRIDE.with(|cell| cell.get()) {
        return result;
    }
    EvaluationResult::Acceptable
}

#[cfg(test)]
thread_local! {
    static TEST_EVAL_OVERRIDE: std::cell::Cell<Option<EvaluationResult>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_eval_result_for_test(result: EvaluationResult) -> EvalOverrideGuard {
    TEST_EVAL_OVERRIDE.with(|cell| cell.set(Some(result)));
    EvalOverrideGuard
}

/// Drop guard that resets [`TEST_EVAL_OVERRIDE`] on drop, even if
/// the test panics.
#[cfg(test)]
#[must_use = "the guard must be held for the duration of the test"]
pub(crate) struct EvalOverrideGuard;

#[cfg(test)]
impl Drop for EvalOverrideGuard {
    fn drop(&mut self) {
        TEST_EVAL_OVERRIDE.with(|cell| cell.set(None));
    }
}

// ---------------------------------------------------------------------------
// Helper: write_back_evaluation
// ---------------------------------------------------------------------------

/// Push an [`Evaluation`](crate::event::SystemEvent::Evaluation) event
/// into the events vector.
///
/// This is the sole path for producing Evaluation events. Persistence
/// to the store is handled by [`execute_task`] via batch append.
pub fn write_back_evaluation(
    task_id: TaskId,
    metric: &str,
    score: f64,
    events: &mut Vec<SystemEvent>,
) {
    events.push(SystemEvent::Evaluation {
        id: task_id,
        metric: metric.to_string(),
        score,
    });
}

// ---------------------------------------------------------------------------
// Helper: with_retry
// ---------------------------------------------------------------------------

/// Execute a fallible operation with exponential backoff.
///
/// Retries up to `max_retries` times with `delay` doubled on each
/// attempt. The `delay` parameter has no default — production code
/// passes a real duration; tests pass [`Duration::ZERO`] for
/// deterministic execution.
///
/// # Blocking
///
/// This function calls [`std::thread::sleep`] between retry attempts,
/// which blocks the calling thread. This is acceptable for the v1
/// synchronous architecture. Async support is deferred to a future
/// release.
pub fn with_retry<T>(
    max_retries: u32,
    delay: Duration,
    mut f: impl FnMut() -> Result<T, Error>,
) -> Result<T, Error> {
    let mut last = f();
    for attempt in 1..=max_retries {
        if last.is_ok() {
            return last;
        }
        let backoff = delay * u32::checked_pow(2, attempt - 1).unwrap_or(u32::MAX);
        std::thread::sleep(backoff);
        last = f();
    }
    last
}

// ---------------------------------------------------------------------------
// State machine: execute_task
// ---------------------------------------------------------------------------

/// Execute a goal through the full seven-state task lifecycle.
///
/// Recursively drives the task from Idle through Planning, Executing,
/// Evaluating, Compressing, and Feedback to Completed. Returns the
/// final [`Task`] record and the complete event log.
///
/// Configuration (goal, depth, delay) is passed via a [`TaskConfig`].
/// The five layer parameters are required because a single type cannot
/// implement [`Layer`] for multiple different event types.
///
/// The returned events Vec is the sole record of state transitions;
/// discarding it silently loses all events.
#[must_use]
pub fn execute_task(
    config: TaskConfig<'_>,
    orch: &mut impl Layer<Event = crate::event::OrchestrationEvent>,
    exec: &mut impl Layer<Event = crate::event::ExecutionEvent>,
    retr: &mut impl Layer<Event = crate::event::RetrievalEvent>,
    mem: &mut impl Layer<Event = crate::event::MemoryEvent>,
    ctx: &mut impl Layer<Event = crate::event::ContextEvent>,
    store: &mut dyn Repository<Event = SystemEvent>,
) -> (Task, Vec<SystemEvent>) {
    // -- Constants for task counter --
    // Relaxed ordering is sufficient because fetch_add is atomic
    // regardless of ordering, and no other memory operations depend
    // on the counter's value — it is only used to produce unique IDs.
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let task_id = TaskId(NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed));

    let mut events: Vec<SystemEvent> = Vec::new();
    let mut task = Task {
        id: task_id,
        parent: None,
        goal: config.goal.to_string(),
        state: TaskState::Idle,
        retry_count: 0,
        completion: None,
        sub_tasks: Vec::new(),
    };

    // -- Depth guard --
    if config.depth >= MAX_DEPTH {
        task.completion = Some(TaskCompletion::Failure("maximum depth exceeded".into()));
        task.state = TaskState::Completed;
        events.push(SystemEvent::TaskFailed {
            id: task_id,
            error: Error::Fatal(format!("depth {} reaches MAX_DEPTH", config.depth)),
        });
        if config.depth == 0 {
            store.append_batch(events.clone());
        }
        return (task, events);
    }

    // ===== Phase 1: Idle → Planning =====
    task.state = TaskState::Planning;
    events.push(SystemEvent::TaskStarted { id: task_id });

    let sub_goals: Vec<String> = if crate::core::is_composite_goal(config.goal) {
        // Decompose via the orchestration layer
        match orch.process(crate::event::OrchestrationEvent, store) {
            Ok((_artifact, side_events)) => {
                events.extend(side_events);
                config
                    .goal
                    .split([';', '\n'])
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            Err(err) => {
                let result = finish_failed(task, events, task_id, err);
                if config.depth == 0 {
                    store.append_batch(result.1.clone());
                }
                return result;
            }
        }
    } else {
        vec![]
    };

    events.push(SystemEvent::TaskDecomposed {
        id: task_id,
        sub_goals: sub_goals.clone(),
    });

    // ===== Phase 2: Executing =====
    task.state = TaskState::Executing;

    if sub_goals.is_empty() {
        // -- Simple goal: execute directly via the execution layer --
        let mut attempt: u32 = 0;
        let exec_result = with_retry(MAX_RETRIES, config.delay, || {
            attempt += 1;
            let (_artifact, side_events) = exec.process(crate::event::ExecutionEvent, store)?;
            events.extend(side_events);
            Ok::<_, Error>(())
        });

        // retry_count = number of failed attempts (0 if first try
        // succeeded, attempts-1 if retries happened).
        task.retry_count = attempt.saturating_sub(1);

        match exec_result {
            Ok(_) => {
                events.push(SystemEvent::TaskStepCompleted {
                    id: task_id,
                    step: 0,
                });
            }
            Err(err) => {
                let result = finish_failed(task, events, task_id, err);
                if config.depth == 0 {
                    store.append_batch(result.1.clone());
                }
                return result;
            }
        }
    } else {
        // -- Composite goal: execute each sub-goal recursively --
        let mut sub_task_ids: Vec<TaskId> = Vec::new();
        let mut has_failure = false;
        let mut sub_task_error_msg = String::new();

        for (i, sg) in sub_goals.iter().enumerate() {
            let (mut sub_task, mut sub_events) = execute_task(
                TaskConfig {
                    goal: sg,
                    depth: config.depth + 1,
                    delay: config.delay,
                },
                orch,
                exec,
                retr,
                mem,
                ctx,
                store,
            );
            sub_task.parent = Some(task_id);
            events.append(&mut sub_events);
            sub_task_ids.push(sub_task.id);

            if let Some(TaskCompletion::Failure(ref msg)) = sub_task.completion {
                sub_task_error_msg.clone_from(msg);
                // Emit TaskBlocked for this failing sub-task
                events.push(SystemEvent::TaskBlocked {
                    id: task_id,
                    reason: format!("sub-task {} failed: {msg}", sub_task.id.0),
                });
                // Emit TaskBlocked for each remaining sibling that was
                // never attempted so event replay can distinguish "never
                // attempted" from "attempted and completed".
                for skipped in &sub_goals[i + 1..] {
                    events.push(SystemEvent::TaskBlocked {
                        id: task_id,
                        reason: format!("sibling sub-task skipped: '{}'", skipped,),
                    });
                }
                has_failure = true;
                break;
            }
        }
        task.sub_tasks = sub_task_ids;

        if has_failure {
            let result = finish_failed(
                task,
                events,
                task_id,
                Error::TaskFailed {
                    id: task_id,
                    cause: Box::new(if sub_task_error_msg.is_empty() {
                        Error::Fatal("sub-task failure propagated".into())
                    } else {
                        Error::Fatal(sub_task_error_msg)
                    }),
                },
            );
            if config.depth == 0 {
                store.append_batch(result.1.clone());
            }
            return result;
        }
    }

    // ===== Phase 3: Evaluating =====
    task.state = TaskState::Evaluating;
    let eval_result = evaluate(&task);
    let eval_score = if eval_result == EvaluationResult::Unacceptable {
        0.0
    } else {
        1.0
    };
    write_back_evaluation(task_id, "v1-quality", eval_score, &mut events);

    // ===== Phase 4: Compressing =====
    task.state = TaskState::Compressing;
    if let Err(err) = ctx.process(crate::event::ContextEvent, store) {
        let result = finish_failed(task, events, task_id, err);
        if config.depth == 0 {
            store.append_batch(result.1.clone());
        }
        return result;
    }
    // Currently hardcoded to 0 — the artifact is discarded above.
    events.push(SystemEvent::ContextCompressed {
        id: task_id,
        before_size: 0,
        after_size: 0,
    });

    // ===== Phase 5: Feedback =====
    task.state = TaskState::Feedback;
    if let Err(err) = retr.process(crate::event::RetrievalEvent, store) {
        let result = finish_failed(task, events, task_id, err);
        if config.depth == 0 {
            store.append_batch(result.1.clone());
        }
        return result;
    }
    if let Err(err) = mem.process(crate::event::MemoryEvent, store) {
        let result = finish_failed(task, events, task_id, err);
        if config.depth == 0 {
            store.append_batch(result.1.clone());
        }
        return result;
    }

    // ===== Phase 6: Completed =====
    task.state = TaskState::Completed;
    if eval_result == EvaluationResult::Unacceptable {
        events.push(SystemEvent::FeedbackGenerated {
            id: task_id,
            insight: "quality threshold not met".into(),
        });
        task.completion = Some(TaskCompletion::Failure("quality threshold not met".into()));
        events.push(SystemEvent::TaskFailed {
            id: task_id,
            error: Error::Fatal("quality threshold not met".into()),
        });
        if config.depth == 0 {
            store.append_batch(events.clone());
        }
        return (task, events);
    }
    task.completion = Some(TaskCompletion::Success);
    events.push(SystemEvent::TaskCompleted {
        id: task_id,
        summary: config.goal.to_string(),
    });

    // Persist all lifecycle events to the store so that fold_events
    // can reconstruct the final state from the store alone.
    // Only the top-level call persists (depth == 0) — recursive calls
    // pass events up to the parent which persists once.
    if config.depth == 0 {
        store.append_batch(events.clone());
    }
    (task, events)
}

/// Helper to mark a task as failed, emit the failure event, and return.
///
/// The caller is responsible for persisting the returned events to the
/// store when this is the top-level call (depth == 0).
#[must_use]
fn finish_failed(
    mut task: Task,
    mut events: Vec<SystemEvent>,
    task_id: TaskId,
    error: Error,
) -> (Task, Vec<SystemEvent>) {
    task.completion = Some(TaskCompletion::Failure(format!("{error}")));
    task.state = TaskState::Completed;
    events.push(SystemEvent::TaskFailed { id: task_id, error });
    (task, events)
}

// ---------------------------------------------------------------------------
// Mock layers (for testing)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod mocks {
    //! Mock layer implementations that delegate to a configurable
    //! function, enabling each test scenario to control behaviour
    //! independently.

    use std::marker::PhantomData;

    use crate::core::{EventPayload, Index, Layer, Observation, Plan, Record, Window};
    use crate::event::{Error, ExecutionEvent, OrchestrationEvent, SystemEvent};
    use crate::store::Repository;

    /// Type alias for the [`Orchestration`](crate::core::Orchestration) layer's process function.
    type OrchFn = Box<
        dyn FnMut(
            OrchestrationEvent,
            &mut dyn Repository<Event = SystemEvent>,
        ) -> Result<(Plan<EventPayload>, Vec<SystemEvent>), Error>,
    >;
    /// Type alias for the [`Execution`](crate::core::Execution) layer's process function.
    type ExecFn = Box<
        dyn FnMut(
            ExecutionEvent,
            &mut dyn Repository<Event = SystemEvent>,
        ) -> Result<(Observation<EventPayload>, Vec<SystemEvent>), Error>,
    >;
    /// Type alias for the [`Retrieval`](crate::core::Retrieval) layer's process function.
    type RetrFn = Box<
        dyn FnMut(
            crate::event::RetrievalEvent,
            &mut dyn Repository<Event = SystemEvent>,
        ) -> Result<(Index<EventPayload>, Vec<SystemEvent>), Error>,
    >;
    /// Type alias for the [`Memory`](crate::core::Memory) layer's process function.
    type MemFn = Box<
        dyn FnMut(
            crate::event::MemoryEvent,
            &mut dyn Repository<Event = SystemEvent>,
        ) -> Result<(Record<EventPayload>, Vec<SystemEvent>), Error>,
    >;
    /// Type alias for the [`Context`](crate::core::Context) layer's process function.
    type CtxFn = Box<
        dyn FnMut(
            crate::event::ContextEvent,
            &mut dyn Repository<Event = SystemEvent>,
        ) -> Result<(Window<EventPayload>, Vec<SystemEvent>), Error>,
    >;

    // -----------------------------------------------------------------------
    // MockOrch
    // -----------------------------------------------------------------------

    /// Mock implementation of the Orchestration layer.
    ///
    /// Delegates [`process`](Layer::process) to a configurable closure.
    pub(crate) struct MockOrch {
        process_fn: OrchFn,
    }

    impl MockOrch {
        /// Create a new mock with the given process implementation.
        pub(crate) fn new(
            f: impl FnMut(
                OrchestrationEvent,
                &mut dyn Repository<Event = SystemEvent>,
            ) -> Result<(Plan<EventPayload>, Vec<SystemEvent>), Error>
            + 'static,
        ) -> Self {
            MockOrch {
                process_fn: Box::new(f),
            }
        }
    }

    impl Default for MockOrch {
        fn default() -> Self {
            MockOrch::new(|_, _| Ok((crate::core::Plan(PhantomData), vec![])))
        }
    }

    impl crate::algebra::TypeConstructor for MockOrch {
        type Of<T: ?Sized> = crate::core::Plan<T>;
    }

    impl Layer for MockOrch {
        type Event = OrchestrationEvent;

        fn process(
            &mut self,
            event: OrchestrationEvent,
            store: &mut dyn Repository<Event = SystemEvent>,
        ) -> Result<(Self::Of<EventPayload>, Vec<SystemEvent>), Error> {
            (self.process_fn)(event, store)
        }
    }

    // -----------------------------------------------------------------------
    // MockExec
    // -----------------------------------------------------------------------

    /// Mock implementation of the Execution layer.
    ///
    /// Delegates [`process`](Layer::process) to a configurable closure.
    pub(crate) struct MockExec {
        process_fn: ExecFn,
    }

    impl MockExec {
        /// Create a new mock with the given process implementation.
        pub(crate) fn new(
            f: impl FnMut(
                ExecutionEvent,
                &mut dyn Repository<Event = SystemEvent>,
            )
                -> Result<(crate::core::Observation<EventPayload>, Vec<SystemEvent>), Error>
            + 'static,
        ) -> Self {
            MockExec {
                process_fn: Box::new(f),
            }
        }
    }

    impl Default for MockExec {
        fn default() -> Self {
            MockExec::new(|_, _| Ok((crate::core::Observation(PhantomData), vec![])))
        }
    }

    impl crate::algebra::TypeConstructor for MockExec {
        type Of<T: ?Sized> = crate::core::Observation<T>;
    }

    impl Layer for MockExec {
        type Event = ExecutionEvent;

        fn process(
            &mut self,
            event: ExecutionEvent,
            store: &mut dyn Repository<Event = SystemEvent>,
        ) -> Result<(Self::Of<EventPayload>, Vec<SystemEvent>), Error> {
            (self.process_fn)(event, store)
        }
    }

    // -----------------------------------------------------------------------
    // MockRetr
    // -----------------------------------------------------------------------

    /// Mock implementation of the Retrieval layer.
    pub(crate) struct MockRetr {
        process_fn: RetrFn,
    }

    impl MockRetr {
        pub(crate) fn new(
            f: impl FnMut(
                crate::event::RetrievalEvent,
                &mut dyn Repository<Event = SystemEvent>,
            )
                -> Result<(crate::core::Index<EventPayload>, Vec<SystemEvent>), Error>
            + 'static,
        ) -> Self {
            MockRetr {
                process_fn: Box::new(f),
            }
        }
    }

    impl Default for MockRetr {
        fn default() -> Self {
            MockRetr::new(|_, _| Ok((crate::core::Index(PhantomData), vec![])))
        }
    }

    impl crate::algebra::TypeConstructor for MockRetr {
        type Of<T: ?Sized> = crate::core::Index<T>;
    }

    impl Layer for MockRetr {
        type Event = crate::event::RetrievalEvent;

        fn process(
            &mut self,
            event: crate::event::RetrievalEvent,
            store: &mut dyn Repository<Event = SystemEvent>,
        ) -> Result<(Self::Of<EventPayload>, Vec<SystemEvent>), Error> {
            (self.process_fn)(event, store)
        }
    }

    // -----------------------------------------------------------------------
    // MockMem
    // -----------------------------------------------------------------------

    /// Mock implementation of the Memory layer.
    pub(crate) struct MockMem {
        process_fn: MemFn,
    }

    impl MockMem {
        pub(crate) fn new(
            f: impl FnMut(
                crate::event::MemoryEvent,
                &mut dyn Repository<Event = SystemEvent>,
            )
                -> Result<(crate::core::Record<EventPayload>, Vec<SystemEvent>), Error>
            + 'static,
        ) -> Self {
            MockMem {
                process_fn: Box::new(f),
            }
        }
    }

    impl Default for MockMem {
        fn default() -> Self {
            MockMem::new(|_, _| Ok((crate::core::Record(PhantomData), vec![])))
        }
    }

    impl crate::algebra::TypeConstructor for MockMem {
        type Of<T: ?Sized> = crate::core::Record<T>;
    }

    impl Layer for MockMem {
        type Event = crate::event::MemoryEvent;

        fn process(
            &mut self,
            event: crate::event::MemoryEvent,
            store: &mut dyn Repository<Event = SystemEvent>,
        ) -> Result<(Self::Of<EventPayload>, Vec<SystemEvent>), Error> {
            (self.process_fn)(event, store)
        }
    }

    // -----------------------------------------------------------------------
    // MockCtx
    // -----------------------------------------------------------------------

    /// Mock implementation of the Context layer.
    pub(crate) struct MockCtx {
        process_fn: CtxFn,
    }

    impl MockCtx {
        pub(crate) fn new(
            f: impl FnMut(
                crate::event::ContextEvent,
                &mut dyn Repository<Event = SystemEvent>,
            )
                -> Result<(crate::core::Window<EventPayload>, Vec<SystemEvent>), Error>
            + 'static,
        ) -> Self {
            MockCtx {
                process_fn: Box::new(f),
            }
        }
    }

    impl Default for MockCtx {
        fn default() -> Self {
            MockCtx::new(|_, _| Ok((crate::core::Window(PhantomData), vec![])))
        }
    }

    impl crate::algebra::TypeConstructor for MockCtx {
        type Of<T: ?Sized> = crate::core::Window<T>;
    }

    impl Layer for MockCtx {
        type Event = crate::event::ContextEvent;

        fn process(
            &mut self,
            event: crate::event::ContextEvent,
            store: &mut dyn Repository<Event = SystemEvent>,
        ) -> Result<(Self::Of<EventPayload>, Vec<SystemEvent>), Error> {
            (self.process_fn)(event, store)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::marker::PhantomData;

    use crate::core::Observation;
    use crate::event::{Error, SystemEvent};
    use crate::store::VecRepository;
    use mocks::*;

    /// Helper: create a store seeded with events.
    fn store() -> VecRepository<SystemEvent> {
        VecRepository::default()
    }

    // =======================================================================
    // Full state transition
    // =======================================================================

    /// A simple goal completes all seven states: Idle → Planning → Executing
    /// → Evaluating → Compressing → Feedback → Completed.
    #[test]
    fn full_state_transition() {
        let mut orch = MockOrch::default();
        let mut exec = MockExec::default();
        let mut retr = MockRetr::default();
        let mut mem = MockMem::default();
        let mut ctx = MockCtx::default();
        let mut repo = store();

        let (task, events) = execute_task(
            TaskConfig {
                goal: "simple goal",
                depth: 0,
                delay: Duration::ZERO,
            },
            &mut orch,
            &mut exec,
            &mut retr,
            &mut mem,
            &mut ctx,
            &mut repo,
        );

        assert_eq!(task.state, TaskState::Completed);
        assert_eq!(task.completion, Some(TaskCompletion::Success));
        assert_eq!(task.goal, "simple goal");

        // Verify key events in the event log
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskStarted { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskStepCompleted { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskCompleted { .. }))
        );
    }

    // =======================================================================
    // Composite goal decomposition
    // =======================================================================

    /// A composite goal (separated by `;`) produces multiple sub-tasks.
    #[test]
    fn composite_goal_spawns_sub_tasks() {
        let mut orch = MockOrch::default();
        let mut exec = MockExec::default();
        let mut retr = MockRetr::default();
        let mut mem = MockMem::default();
        let mut ctx = MockCtx::default();
        let mut repo = store();

        let (task, events) = execute_task(
            TaskConfig {
                goal: "step1; step2; step3",
                depth: 0,
                delay: Duration::ZERO,
            },
            &mut orch,
            &mut exec,
            &mut retr,
            &mut mem,
            &mut ctx,
            &mut repo,
        );

        assert_eq!(task.state, TaskState::Completed);
        assert_eq!(task.completion, Some(TaskCompletion::Success));
        // The parent task should have sub-tasks
        assert!(
            !task.sub_tasks.is_empty(),
            "composite goal should spawn sub-tasks"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskDecomposed { .. }))
        );
    }

    // =======================================================================
    // Sub-task failure propagates
    // =======================================================================

    /// When a sub-task fails, the parent is marked as blocked.
    #[test]
    fn sub_task_failure_triggers_blocked() {
        let mut orch = MockOrch::default();
        // Exec layer fails on first call (sub-tasks' exec)
        let mut exec = MockExec::new(|_, _| Err(Error::Fatal("exec failed".into())));
        let mut retr = MockRetr::default();
        let mut mem = MockMem::default();
        let mut ctx = MockCtx::default();
        let mut repo = store();

        let (task, events) = execute_task(
            TaskConfig {
                goal: "step1; step2",
                depth: 0,
                delay: Duration::ZERO,
            },
            &mut orch,
            &mut exec,
            &mut retr,
            &mut mem,
            &mut ctx,
            &mut repo,
        );

        assert_eq!(task.state, TaskState::Completed);
        assert!(matches!(task.completion, Some(TaskCompletion::Failure(_))));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskBlocked { .. }))
        );
    }

    // =======================================================================
    // Retry escalation
    // =======================================================================

    /// A task that fails `MAX_RETRIES + 1` times ends as `Failure`.
    #[test]
    fn retry_escalation_after_max_retries() {
        let mut orch = MockOrch::default();
        let mut exec = MockExec::new(|_, _| Err(Error::Transient("transient failure".into())));
        let mut retr = MockRetr::default();
        let mut mem = MockMem::default();
        let mut ctx = MockCtx::default();
        let mut repo = store();

        // A non-composite goal goes through exec, which fails every time
        let (task, events) = execute_task(
            TaskConfig {
                goal: "flaky",
                depth: 0,
                delay: Duration::ZERO,
            },
            &mut orch,
            &mut exec,
            &mut retr,
            &mut mem,
            &mut ctx,
            &mut repo,
        );

        assert_eq!(task.state, TaskState::Completed);
        assert!(matches!(task.completion, Some(TaskCompletion::Failure(_))));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskFailed { .. }))
        );
    }

    // =======================================================================
    // Retry then succeed
    // =======================================================================

    /// A task that fails once then succeeds on retry.
    #[test]
    fn retry_then_succeed() {
        let mut orch = MockOrch::default();
        let mut call_count = 0;
        let mut exec = MockExec::new(move |_, _| {
            call_count += 1;
            if call_count == 1 {
                Err(Error::Transient("first attempt failed".into()))
            } else {
                Ok((Observation(PhantomData), vec![]))
            }
        });
        let mut retr = MockRetr::default();
        let mut mem = MockMem::default();
        let mut ctx = MockCtx::default();
        let mut repo = store();

        let (task, events) = execute_task(
            TaskConfig {
                goal: "retry-me",
                depth: 0,
                delay: Duration::ZERO,
            },
            &mut orch,
            &mut exec,
            &mut retr,
            &mut mem,
            &mut ctx,
            &mut repo,
        );

        assert_eq!(task.state, TaskState::Completed);
        assert_eq!(task.completion, Some(TaskCompletion::Success));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskCompleted { .. }))
        );
    }

    // =======================================================================
    // Depth guard
    // =======================================================================

    /// A task at `MAX_DEPTH` immediately completes as `Failure`.
    #[test]
    fn depth_guard_prevents_excessive_nesting() {
        let mut orch = MockOrch::default();
        let mut exec = MockExec::default();
        let mut retr = MockRetr::default();
        let mut mem = MockMem::default();
        let mut ctx = MockCtx::default();
        let mut repo = store();

        let (task, events) = execute_task(
            TaskConfig {
                goal: "deep",
                depth: MAX_DEPTH,
                delay: Duration::ZERO,
            },
            &mut orch,
            &mut exec,
            &mut retr,
            &mut mem,
            &mut ctx,
            &mut repo,
        );

        assert_eq!(task.state, TaskState::Completed);
        let depth_err = match &task.completion {
            Some(TaskCompletion::Failure(msg)) => msg.contains("depth"),
            _ => false,
        };
        assert!(
            depth_err,
            "expected depth-related failure, got {:?}",
            task.completion
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskFailed { .. }))
        );
    }

    /// A task at `MAX_DEPTH - 1` succeeds (boundary test).
    #[test]
    fn depth_guard_allows_max_depth_minus_one() {
        let mut orch = MockOrch::default();
        let mut exec = MockExec::default();
        let mut retr = MockRetr::default();
        let mut mem = MockMem::default();
        let mut ctx = MockCtx::default();
        let mut repo = store();

        let (task, events) = execute_task(
            TaskConfig {
                goal: "simple",
                depth: MAX_DEPTH - 1,
                delay: Duration::ZERO,
            },
            &mut orch,
            &mut exec,
            &mut retr,
            &mut mem,
            &mut ctx,
            &mut repo,
        );

        assert_eq!(task.state, TaskState::Completed);
        assert_eq!(
            task.completion,
            Some(TaskCompletion::Success),
            "task at MAX_DEPTH - 1 should succeed, got {:?}",
            task.completion,
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskCompleted { .. })),
            "expected TaskCompleted event, got events: {events:?}",
        );
    }

    // =======================================================================
    // is_composite_goal
    // =======================================================================

    #[test]
    fn is_composite_goal_detects_simple_goals() {
        assert!(!crate::core::is_composite_goal("simple"));
        assert!(!crate::core::is_composite_goal("a single instruction"));
    }

    #[test]
    fn is_composite_goal_detects_compound_goals() {
        assert!(crate::core::is_composite_goal("step1; step2"));
        assert!(crate::core::is_composite_goal("line1\nline2"));
        assert!(crate::core::is_composite_goal("a;b;c"));
    }

    // =======================================================================
    // EvaluationResult constructibility
    // =======================================================================

    #[test]
    fn evaluation_result_constructible() {
        let acceptable = EvaluationResult::Acceptable;
        let unacceptable = EvaluationResult::Unacceptable;
        assert_ne!(acceptable, unacceptable);
    }

    // =======================================================================
    // with_retry exists
    // =======================================================================

    #[test]
    fn with_retry_succeeds_on_first_try() {
        let result = with_retry(3, Duration::ZERO, || Ok::<_, Error>(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn with_retry_succeeds_on_retry() {
        let mut call_count = 0;
        let result = with_retry(3, Duration::ZERO, || {
            call_count += 1;
            if call_count < 2 {
                Err(Error::Transient("not yet".into()))
            } else {
                Ok::<_, Error>(99)
            }
        });
        assert_eq!(result.unwrap(), 99);
    }

    #[test]
    fn with_retry_exhaustion_fails() {
        let result = with_retry(2, Duration::ZERO, || {
            Err::<(), _>(Error::Transient("always fails".into()))
        });
        assert!(result.is_err());
    }

    #[test]
    fn with_retry_zero_max_retries_fails_immediately() {
        let result = with_retry(0, Duration::ZERO, || {
            Err::<(), _>(Error::Transient("fail".into()))
        });
        assert!(result.is_err());
    }

    #[test]
    fn with_retry_zero_max_retries_succeeds() {
        let result = with_retry(0, Duration::ZERO, || Ok::<_, Error>(42));
        assert_eq!(result.unwrap(), 42);
    }

    // =======================================================================
    // Integration: leaf task e2e
    // =======================================================================

    /// End-to-end state machine integration test.
    ///
    /// Runs a leaf task through all seven states using stub mock layers
    /// (no actual work — validates framework control flow). Verifies:
    ///
    /// - Task completes as `Completed(Success)`
    /// - Event log contains all lifecycle event types
    /// - [`fold_events`](crate::store::fold_events) replays to `Completed`
    /// - [`has_failed`](crate::store::Repository::has_failed) returns `false`
    #[test]
    fn state_machine_integration() {
        let mut orch = MockOrch::default();
        let mut exec = MockExec::default();
        let mut retr = MockRetr::default();
        let mut mem = MockMem::default();
        let mut ctx = MockCtx::default();
        let mut repo = store();

        let (task, events) = execute_task(
            TaskConfig {
                goal: "hello",
                depth: 0,
                delay: Duration::ZERO,
            },
            &mut orch,
            &mut exec,
            &mut retr,
            &mut mem,
            &mut ctx,
            &mut repo,
        );

        // Task completed successfully
        assert_eq!(task.state, TaskState::Completed);
        assert_eq!(task.completion, Some(TaskCompletion::Success));
        assert_eq!(task.retry_count, 0);

        // All lifecycle event types present
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskStarted { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskDecomposed { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskStepCompleted { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::ContextCompressed { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::Evaluation { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskCompleted { .. }))
        );

        // fold_events replays to Completed
        assert_eq!(
            crate::store::fold_events(&events, task.id),
            Some(TaskState::Completed),
        );

        // has_failed returns false for the completed task
        assert!(!repo.has_failed(task.id));

        // Unrelated task IDs don't affect replay state
        assert_eq!(crate::store::fold_events(&events, TaskId(999)), None);

        // All lifecycle events were persisted to the store
        let store_events = repo.stream(0);
        assert_eq!(store_events.len(), events.len());
        // fold_events on the store returns the same result as on the Vec
        assert_eq!(
            crate::store::fold_events(&store_events, task.id),
            crate::store::fold_events(&events, task.id),
            "store and returned event log must agree on final state",
        );
    }

    // =======================================================================
    // Layer failure during Compressing phase
    // =======================================================================

    /// A failing ctx layer during Compressing produces TaskFailure.
    #[test]
    fn ctx_failure_during_compressing() {
        let mut ctx = MockCtx::new(|_, _| Err(Error::Fatal("ctx failure".into())));
        let (task, events) = execute_task(
            TaskConfig {
                goal: "simple",
                depth: 0,
                delay: Duration::ZERO,
            },
            &mut MockOrch::default(),
            &mut MockExec::default(),
            &mut MockRetr::default(),
            &mut MockMem::default(),
            &mut ctx,
            &mut store(),
        );

        assert_eq!(task.state, TaskState::Completed);
        assert!(matches!(task.completion, Some(TaskCompletion::Failure(_))));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskFailed { .. })),
            "ctx failure must produce TaskFailed event",
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SystemEvent::ContextCompressed { .. })),
            "ctx failure must not emit ContextCompressed",
        );
    }

    // =======================================================================
    // Layer failure during Feedback phase (retr)
    // =======================================================================

    /// A failing retr layer during Feedback produces TaskFailure.
    #[test]
    fn retr_failure_during_feedback() {
        let mut retr = MockRetr::new(|_, _| Err(Error::Fatal("retr failure".into())));
        let (task, events) = execute_task(
            TaskConfig {
                goal: "simple",
                depth: 0,
                delay: Duration::ZERO,
            },
            &mut MockOrch::default(),
            &mut MockExec::default(),
            &mut retr,
            &mut MockMem::default(),
            &mut MockCtx::default(),
            &mut store(),
        );

        assert_eq!(task.state, TaskState::Completed);
        assert!(matches!(task.completion, Some(TaskCompletion::Failure(_))));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskFailed { .. })),
            "retr failure must produce TaskFailed event",
        );
    }

    // =======================================================================
    // Layer failure during Feedback phase (mem)
    // =======================================================================

    /// A failing mem layer during Feedback produces TaskFailure.
    #[test]
    fn mem_failure_during_feedback() {
        let mut mem = MockMem::new(|_, _| Err(Error::Fatal("mem failure".into())));
        let (task, events) = execute_task(
            TaskConfig {
                goal: "simple",
                depth: 0,
                delay: Duration::ZERO,
            },
            &mut MockOrch::default(),
            &mut MockExec::default(),
            &mut MockRetr::default(),
            &mut mem,
            &mut MockCtx::default(),
            &mut store(),
        );

        assert_eq!(task.state, TaskState::Completed);
        assert!(matches!(task.completion, Some(TaskCompletion::Failure(_))));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskFailed { .. })),
            "mem failure must produce TaskFailed event",
        );
    }

    // =======================================================================
    // Evaluation Unacceptable path
    // =======================================================================

    /// When evaluate returns Unacceptable, the task completes as Failure
    /// with a TaskFailed event (not TaskCompleted).
    #[test]
    fn evaluate_unacceptable_produces_failure() {
        // The guard resets the override when dropped, even on panic.
        let _guard = set_eval_result_for_test(EvaluationResult::Unacceptable);

        let (task, events) = execute_task(
            TaskConfig {
                goal: "simple",
                depth: 0,
                delay: Duration::ZERO,
            },
            &mut MockOrch::default(),
            &mut MockExec::default(),
            &mut MockRetr::default(),
            &mut MockMem::default(),
            &mut MockCtx::default(),
            &mut store(),
        );

        assert_eq!(task.state, TaskState::Completed);
        assert!(
            matches!(task.completion, Some(TaskCompletion::Failure(_))),
            "unacceptable evaluation should produce Failure, got {:?}",
            task.completion,
        );

        // Must have FeedbackGenerated before TaskFailed
        let fb_pos = events
            .iter()
            .position(|e| matches!(e, SystemEvent::FeedbackGenerated { .. }));
        let tf_pos = events
            .iter()
            .position(|e| matches!(e, SystemEvent::TaskFailed { .. }));
        assert!(
            fb_pos.is_some() && tf_pos.is_some() && fb_pos < tf_pos,
            "FeedbackGenerated must precede TaskFailed in event order",
        );

        // Must NOT have TaskCompleted
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SystemEvent::TaskCompleted { .. })),
            "unacceptable path must not emit TaskCompleted",
        );
    }
}
