# Interactive Agent Session Plan

## Verdict

HayCut is already more than a harness skeleton: it can classify tasks, detect projects, run baselines, gather evidence, select context, generate structured edits, apply patches, verify results, retry within budgets, and record a task flight.

The next milestone is to turn that constrained pipeline into a real coding-agent control loop. The core gap is that context planning currently produces one effective action, executes one read/search/trace/pull step, then proceeds to patch planning. A coding agent needs to repeatedly plan, act, observe, and re-plan until it explicitly decides to patch, ask, finish, or stop.

## Goal

Build an `Interactive Agent Session` milestone that introduces a durable engine control API, a repeating planner/action loop, persisted user interactions, approval gates, and a small line-oriented terminal session.

The first usable interface should look roughly like this:

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
- Do not extend the dashboard into a controller yet.
- Do not broaden autonomous file editing before the control loop and approval model are in place.
- Do not keep building around stringified shell-like action commands as the internal contract.

## Assumptions To Validate Before Coding

These names are based on the current behavior description and should be verified in the repository before implementation:

- `PlanContext` is the step that asks the planner what context/action is needed next.
- `ReadContext` or nearby workflow code executes the planner-selected context action.
- `needs_more_context()` is currently route-driven and becomes false once `PlanContext` and `ReadContext` have both appeared.
- `execute_plan_context()` treats `finish` and `ask` as terminal locally, but the outer DAG can still continue to patch generation because that decision is not persisted as workflow state.
- `NextAction.command: String` is the current bridge from planner output to action execution.
- The dashboard is observer-only because server routes are currently read-only GET endpoints.

If any of these are wrong, preserve the plan intent but adjust the owning modules and names.

## Target Architecture

### Engine Events

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

### Control Commands

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

### Engine API

All interfaces should drive the same engine operations.

```rust
impl AgentEngine {
    fn advance(&mut self, task_id: TaskId, command: ControlCommand) -> Result<Vec<AgentEvent>>;
    fn run_until_blocked(&mut self, task_id: TaskId) -> Result<Vec<AgentEvent>>;
}
```

`advance()` should execute one control decision and return the events produced by that decision.

`run_until_blocked()` should continue until the task needs human input, approval, a risky command decision, or has finished.

## Typed Actions

Replace stringified `NextAction.command` handling with typed actions that stay typed from planner output through execution.

```rust
enum AgentAction {
    Search { query: String },
    ReadSymbol { target: String },
    ReadWindow { path: PathBuf, line: usize, radius: usize },
    RunCommand { program: String, args: Vec<String> },
    PullContext { id: String },
    PlanPatch,
    AskUser { question: String },
    Finish,
}
```

The planner can still receive or emit a serializable schema, but the internal workflow should not serialize typed actions into strings and parse them back again. This avoids quoting bugs, paths with spaces, and impossible states such as an unrecognized queued action string.

## Required Workflow Semantics

The core loop should become:

```text
PlanContext
  -> AgentAction proposed
  -> action approved or auto-allowed
  -> execute action
  -> record observation
  -> PlanContext again
  -> repeat until the planner explicitly chooses PlanPatch, AskUser, Finish, or Stop
```

Transitions should be result-driven rather than route-history-driven:

| Planner/action result | Next workflow state |
| --- | --- |
| `Search`, `ReadSymbol`, `ReadWindow`, `RunCommand`, `PullContext` | execute, record observation, return to `PlanContext` |
| `PlanPatch` | move to patch planning/generation |
| `AskUser` | persist pending interaction and block |
| `Finish` | move to report/final outcome |
| `Stop` | persist stopped state |

`needs_more_context()` should no longer infer readiness from whether step names have appeared in the route. Readiness must be an explicit planner or engine decision.

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

## Approval Model

Require approval before applying patches and before running non-trivial commands.

Command risk policy:

| Risk | Default behavior | Examples |
| --- | --- | --- |
| Low | auto-allow | tests, build, lint, formatting checks, read-only Git |
| Medium | ask first | package installation, code generation, migrations, commands that modify tracked files |
| High | deny by default | destructive filesystem operations, destructive Git operations, network publishing, credential or secret access |

Add command timeout and cancellation support. Every command event should record command, args, working directory, timeout, exit status, duration, stdout/stderr summary, and whether it was approved.

## Patch Vocabulary

Keep exact anchor replacement because it is a good low-token and safe representation, but extend the vocabulary beyond existing-file replacement.

```rust
enum FileEdit {
    Replace { path: PathBuf, find: String, replace: String, expected_digest: FileDigest },
    Create { path: PathBuf, content: String },
    Delete { path: PathBuf, expected_digest: FileDigest },
    Rename { from: PathBuf, to: PathBuf, expected_digest: FileDigest },
}
```

Validation rules:

- `Replace` anchors must match exactly once.
- `Create` must fail if the target already exists unless explicitly approved as overwrite.
- `Delete` and `Rename` require an expected digest.
- All file operations must be previewable before approval.
- Patch application should produce structured success/failure events.

## Working-Tree Ownership

Move from "refuse any file with uncommitted changes" to optimistic concurrency.

Desired contract:

- Record file digest when context is read.
- Allow pre-existing user changes.
- Apply only if the current digest matches the digest used for patch planning.
- Refuse if the file changed after the agent inspected it.
- Report conflicts as recoverable observations and return to planning.
- Later, add an option for isolated temporary Git worktrees in highly autonomous mode.

## Verification

Make verification first-class and user-overridable.

```rust
struct VerificationPlan {
    checks: Vec<VerificationCheck>,
}

struct VerificationCheck {
    command: VerificationCommand,
    required: bool,
    scope: VerificationScope,
}

enum VerificationScope {
    FullProject,
    ChangedFilesOnly,
    Targeted,
}
```

Examples:

```text
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

`task start --verify` should populate structured verification checks, not only acceptance-criteria text. Project detection can provide defaults, but user-provided verification must override or augment those defaults.

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

After the engine control API is stable, the existing dashboard can become a controller by adding mutating routes that submit `ControlCommand`s. Until then, keep it as an observer.

Potential future routes:

- `POST /tasks/:id/advance`
- `POST /tasks/:id/approve`
- `POST /tasks/:id/reject`
- `POST /tasks/:id/steer`
- `POST /tasks/:id/context`
- `POST /tasks/:id/stop`

## Implementation Phases

### Phase 1: Locate and Lock the Current Behavior

- Add focused tests around the current single-action context behavior.
- Add tests showing that `AskUser` and `Finish` currently do not persist a workflow-blocking decision, if that is the actual behavior.
- Identify the smallest owning modules for planner output, action execution, task persistence, and workflow transition selection.

Exit criteria:

- There is a failing or characterization test for the one-action investigation limit.
- There is a failing or characterization test for `AskUser`/`Finish` persistence behavior.

### Phase 2: Introduce Typed Pending Actions

- Add `AgentAction` and conversion from planner schema to typed actions.
- Replace queued string command parsing with typed action execution.
- Keep any existing serialized task format backward-compatible with a migration or compatibility reader if persisted tasks already exist.
- Remove or quarantine the "unrecognized queued action" state from normal execution.

Exit criteria:

- Existing search/read/trace/pull actions still execute.
- Paths with spaces and quoted command arguments are represented correctly in tests.
- No internal action executor depends on splitting a shell-like string.

### Phase 3: Make Planning Iterative

- Replace route-history `needs_more_context()` readiness with explicit `AgentAction` results.
- Route context-gathering actions back to `PlanContext` after observation recording.
- Route `PlanPatch`, `AskUser`, and `Finish` to their explicit next states.
- Persist terminal and blocked states before returning control to the outer workflow.

Exit criteria:

- A task can perform at least two consecutive context actions before patch planning.
- `PlanPatch` is the only planner action that enters patch generation.
- `AskUser` blocks with the question preserved.
- `Finish` reports without generating a patch.

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

### Phase 7: Approval, Command Policy, and Timeouts

- Classify commands by risk.
- Auto-allow low-risk commands.
- Require approval for medium-risk commands.
- Deny high-risk commands by default.
- Add process timeout and cancellation behavior.

Exit criteria:

- Risk policy is tested with representative commands.
- Long-running commands time out predictably.
- Denied commands become planner-visible observations.

### Phase 8: Expand Patch Vocabulary

- Add `Create`, `Delete`, and `Rename` edits.
- Add digest checks to all destructive or replacement operations.
- Preview all edit types before approval.
- Keep exact unique anchors for replacement edits.

Exit criteria:

- Feature tasks can create new files.
- Delete and rename operations are digest-protected.
- Patch approval displays all file operations clearly.

### Phase 9: Improve Working-Tree Ownership

- Record digests when reading context.
- Permit pre-existing dirty files if the agent has a digest for the inspected version.
- Refuse to apply if the file changed after inspection.
- Return conflicts to the planner as observations.

Exit criteria:

- User edits made before agent inspection do not block work.
- User edits made after agent inspection cause a recoverable conflict.
- Conflict behavior is covered by tests.

### Phase 10: First-Class Verification

- Store structured verification plans.
- Make `task start --verify` populate verification checks.
- Allow `verify <command>` in the REPL to add or run checks.
- Support required, optional, and changed-files-only checks.

Exit criteria:

- User verification commands override or augment project defaults.
- Verification results are persisted and emitted as `AgentEvent::VerificationCompleted`.
- Patch approval/final report can distinguish required and optional check outcomes.

## Suggested Test Strategy

- Planner loop integration test with multiple context actions before patching.
- `AskUser` persistence and resume test.
- `Finish` terminal routing test.
- Typed action tests for paths with spaces and command args containing quotes.
- Command risk policy tests.
- Patch vocabulary tests for replace/create/delete/rename.
- Digest conflict tests for optimistic concurrency.
- REPL command parsing tests that assert emitted `ControlCommand`s.

## Dogfooding Criteria

Start using HayCut on HayCut once these are true:

- The context loop can iterate until explicit `PlanPatch`.
- The terminal session can approve, reject, steer, inspect status, and resume.
- Patches require approval.
- Non-trivial commands require approval or are denied.
- At least one structured verification check runs after edits.
- Failed or rejected proposals return to planning with the reason preserved.

## Recommended Build Order

1. Characterize the current one-action loop.
2. Introduce typed `AgentAction` without changing behavior.
3. Change workflow transitions to loop back to `PlanContext` after context observations.
4. Persist `AskUser`, `Finish`, approvals, rejections, and steering.
5. Add `AgentEngine::advance()` and `run_until_blocked()`.
6. Add the line-oriented REPL.
7. Add command policy and timeouts.
8. Add create/delete/rename edits.
9. Add optimistic concurrency.
10. Upgrade verification into structured checks.

## Open Questions

- Should `Step` mean "execute exactly one workflow node" or "execute exactly one proposed agent action"?
- Should patch approval happen before generation, before application, or both?
- How should planner-visible steering be prioritized relative to original task instructions and acceptance criteria?
- What persisted task schema compatibility is required for existing task records?
- Should command risk policy be configurable per repository?
- Should the first REPL support resuming by task id, or only continue the latest blocked task?