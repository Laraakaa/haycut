# Sub-Plan 2: Engine Control API, Persistence, and the Terminal REPL

Part of the [Interactive Agent Session](plan.md) milestone. Covers Phases 4-6 of the recommended build order. Depends on the typed `AgentAction` loop from [plan_1_typed_actions_and_loop.md](plan_1_typed_actions_and_loop.md).

Shared context: the engine-facing event/command API, durable task state for pending questions/approvals/steering, and the line-oriented terminal session that drives it. These three pieces share one contract — `AgentEvent` in, `ControlCommand` out — so they're easiest to design and test together.

## Verdict / Why This Matters

Build an `Interactive Agent Session` milestone that introduces a durable engine control API, a repeating planner/action loop, persisted user interactions, approval gates, and a small line-oriented terminal session.

Target interface:

```text
$ haycut agent session "Fix the flaky cache invalidation test"

[detect] Rust workspace
[baseline] cargo test ... failed
[evidence] assertion failed at src/cache.rs:184
[agent] wants to read symbol Cache::invalidate

haycut> continue

[agent] proposes 2 edits
haycut> approve
haycut> reject The public API must remain unchanged
haycut> steer Check whether the generation counter can overflow
haycut> context src/cache.rs::Cache
haycut> status
haycut> stop
```

## Non-Goals

- Do not start with a full-screen TUI.
- Do not extend the dashboard into a controller yet (see [Dashboard Follow-Up](#dashboard-follow-up)).
- Do not keep building around stringified shell-like action commands as the internal contract (handled in sub-plan 1).

## Engine Events

Introduce an engine-facing event stream. The CLI, future TUI, dashboard controller, and MCP-style interface should all consume the same events.

```rust
enum AgentEvent {
    Progress(ProgressEvent),
    ActionProposed(AgentAction),
    ApprovalRequired(ApprovalRequest),
    Question(PendingQuestion),
    VerificationCompleted(VerificationResult),
    PatchProposed(PatchProposal),
    Finished(TaskOutcome),
    Stopped(StopReason),
}
```

## Control Commands

Expose a small command vocabulary for human or external control.

```rust
enum ControlCommand {
    Continue,
    Step,
    Approve,
    Reject { reason: String },
    Steer { message: String },
    AddContext { target: ContextTarget },
    Verify { command: VerificationCommand },
    Stop,
}
```

## Engine API

All interfaces should drive the same engine operations.

```rust
impl AgentEngine {
    fn advance(&mut self, task_id: TaskId, command: ControlCommand) -> Result<Vec<AgentEvent>>;
    fn run_until_blocked(&mut self, task_id: TaskId) -> Result<Vec<AgentEvent>>;
}
```

`advance()` should execute one control decision and return the events produced by that decision.

`run_until_blocked()` should continue until the task needs human input, approval, a risky command decision, or has finished.

## Persistent Task State

Add enough durable state to resume an interactive task after process restart.

```rust
struct AgentTaskState {
    pending_action: Option<AgentAction>,
    pending_interaction: Option<PendingInteraction>,
    pending_approval: Option<ApprovalRequest>,
    messages: Vec<TaskMessage>,
    observations: Vec<Observation>,
    steering: Vec<UserConstraint>,
}
```

Required persistence behavior:

- `AskUser` stores the question and blocks.
- A user reply clears `pending_interaction`, appends a task message, and resumes the same task.
- `Reject { reason }` records the rejection as an observation or constraint and returns to planning.
- `Steer { message }` records a durable user constraint and returns to planning.
- `Finish` persists a terminal outcome instead of allowing the DAG to continue into patch planning.
- Restarting HayCut must not lose pending questions, approvals, rejections, steering messages, or proposed actions.

## Terminal REPL

Add a line-oriented session command after the engine API exists.

Initial command set:

| Command | Behavior |
| --- | --- |
| `continue` | run until blocked |
| `step` | execute one engine advance |
| `approve` | approve pending patch/action |
| `reject <reason>` | reject pending proposal and record reason |
| `steer <instruction>` | add a durable instruction or constraint |
| `context <path-or-symbol>` | request additional context |
| `verify <command>` | add/run a verification check |
| `status` | print task state summary |
| `trace` | print recent events/model calls/actions |
| `stop` | persist stopped state and exit |

The REPL should be intentionally boring: line in, events out. Avoid terminal UI complexity until the control contract has survived dogfooding.

## Dashboard Follow-Up

After the engine control API is stable, the existing dashboard can become a controller by adding mutating routes that submit `ControlCommand`s. Until then, keep it as an observer (currently read-only GET endpoints).

Potential future routes:

- `POST /tasks/:id/advance`
- `POST /tasks/:id/approve`
- `POST /tasks/:id/reject`
- `POST /tasks/:id/steer`
- `POST /tasks/:id/context`
- `POST /tasks/:id/stop`

## Implementation Phases

### Phase 4: Add Engine Control API

- Add `AgentEvent` and `ControlCommand`.
- Implement `advance()` for one decision at a time.
- Implement `run_until_blocked()` for autonomous progress until human input is required.
- Ensure every state transition emits durable, inspectable events.

Exit criteria:

- Unit or integration tests can drive a task using only `ControlCommand`s.
- The engine can stop at pending approval, pending question, failed verification, finished state, or explicit stop.

### Phase 5: Persist User Interaction

- Add `pending_interaction`, `pending_approval`, `messages`, and steering/constraint storage.
- Resume blocked tasks from persisted state.
- Treat rejections and steering as planner-visible observations or constraints.

Exit criteria:

- Restarting the process does not lose a pending question or approval.
- Rejecting a patch with a reason causes the next planning pass to see that reason.
- Steering instructions affect subsequent planner prompts or context selection.

### Phase 6: Add Terminal Session

- Add `haycut agent session <task>`.
- Render engine events as compact line-oriented output.
- Implement `continue`, `step`, `approve`, `reject`, `steer`, `context`, `verify`, `status`, `trace`, and `stop`.
- Keep the CLI thin: parsing user commands should translate into `ControlCommand`s, not own workflow policy.

Exit criteria:

- A user can start a task, inspect proposed actions, approve/reject/steer, run verification, and stop/resume from the terminal.
- The CLI and tests exercise the same engine API.

## Test Strategy

- `AskUser` persistence and resume test.
- Command risk policy interplay with `Approve`/`Reject` (policy itself defined in [plan_3_safety_and_execution.md](plan_3_safety_and_execution.md)).
- REPL command parsing tests that assert emitted `ControlCommand`s.

## Dogfooding Criteria

- The context loop can iterate until explicit `PlanPatch` (from sub-plan 1).
- The terminal session can approve, reject, steer, inspect status, and resume.
- Failed or rejected proposals return to planning with the reason preserved.

## Open Questions

- Should `Step` mean "execute exactly one workflow node" or "execute exactly one proposed agent action"?
- Should patch approval happen before generation, before application, or both? (patch mechanics live in [plan_4_patch_and_verification.md](plan_4_patch_and_verification.md))
- How should planner-visible steering be prioritized relative to original task instructions and acceptance criteria?
- Should the first REPL support resuming by task id, or only continue the latest blocked task?

## Handoff

This engine and REPL are the surface that [plan_3_safety_and_execution.md](plan_3_safety_and_execution.md)'s approval/risk policy and [plan_4_patch_and_verification.md](plan_4_patch_and_verification.md)'s patch/verification events plug into.
