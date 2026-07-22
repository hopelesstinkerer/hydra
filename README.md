# hydra

Point it at a goal. That's it.

hydra is a recursive system that captures experience, externalizes
memory, evaluates outcomes, and writes back into itself after every
execution. It learns from every execution, retains knowledge across
time, and improves with every cycle.

## Architecture

hydra is organised around five architectural layers, each wrapped by
a phantom type that connects it to the type system:

| Layer | Marker | Wrapper | Event type | Role |
|-------|--------|---------|------------|------|
| Orchestration | `Orchestration` | `Plan<T>` | `OrchestrationEvent` | Planning, decomposition, dispatching |
| Retrieval | `Retrieval` | `Index<T>` | `RetrievalEvent` | Fetching external data and knowledge |
| Memory | `Memory` | `Record<T>` | `MemoryEvent` | Storing, recalling, compressing history |
| Context | `Context` | `Window<T>` | `ContextEvent` | Window management, context assembly |
| Execution | `Execution` | `Observation<T>` | `ExecutionEvent` | Running tools, executing code |

Each layer implements the `Layer` trait (`TypeConstructor + Sized`)
with an associated `Event` type and a `process` method that takes
a layer-specific event and a mutable event store, returning an
artifact and optional side-effect events.

### Task state machine

Every goal drives a task through seven states:

```
Idle → Planning → Executing → Evaluating → Compressing → Feedback → Completed
```

- **Idle**: Task record created.
- **Planning**: Goal is checked for composition (`;` / `\n` separators).
  Composite goals are decomposed into sub-goals and executed
  recursively; simple goals execute directly.
- **Executing**: The execution layer runs the goal. On transient
  failure the task retries with exponential backoff (up to 3
  attempts). Sub-task failures propagate to the parent.
- **Evaluating**: Output quality is assessed.
- **Compressing**: Context history is compacted.
- **Feedback**: Self-improvement insights are recorded.
- **Completed**: Terminal state (Success or Failure).

### Event sourcing

All state transitions are recorded as `SystemEvent` values in an
append-only `Repository`. The `fold_events` function reconstructs
the last known task state from the event log by replaying lifecycle
events in order:

| Event(s) | Resulting state |
|----------|----------------|
| `TaskStarted` | `Planning` |
| `TaskDecomposed` | `Executing` |
| `TaskStepCompleted` | `Evaluating` |
| `ContextCompressed` | `Feedback` |
| `TaskCompleted` / `TaskFailed` / `TaskBlocked` | `Completed` |

This dual model (write-ahead event log + fold-based read model)
enables full replay after restart and supports audit, debugging,
and recovery without snapshots.

## Module reference

| Module | Contents |
|--------|----------|
| [`algebra`](src/algebra.rs) | `TypeConstructor`, `Semigroup`, `Monoid` traits and type-level composition |
| [`core`](src/core.rs) | Five layer markers, phantom wrappers, `Layer` trait, `EventPayload` |
| [`event`](src/event.rs) | `TaskId`, `SystemEvent` (10 variants), `Error`, per-layer event types |
| [`store`](src/store.rs) | `Repository` trait, `VecRepository`, `fold_events` |
| [`task`](src/task.rs) | `TaskState`, `Task`, `execute_task` state machine, mock layers |

## Building

```bash
cargo build
cargo test
cargo run
```
