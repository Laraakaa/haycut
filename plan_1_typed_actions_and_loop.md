# Sub-Plan 1: Typed Actions and the Iterative Planning Loop

Part of the [Interactive Agent Session](plan.md) milestone. Covers Phases 1-3 of the recommended build order: characterizing current behavior, replacing stringified actions with typed ones, and making context planning loop until an explicit terminal decision.

Shared context: planner output (`NextAction.command: String`), `execute_plan_context()`, `needs_more_context()`, and workflow transition selection in the agent DAG. This is the foundational layer everything else (engine API, persistence, REPL) is built on top of, so it should land first and change runtime behavior least.

## Verdict / Why This Matters

Context planning currently produces one effective action, executes one read/search/trace/pull step, then proceeds to patch planning. A coding agent needs to repeatedly plan, act, observe, and re-plan until it explicitly decides to patch, ask, finish, or stop.

## Assumptions To Validate Before Coding

- `PlanContext` is the step that asks the planner what context/action is needed next.
- `ReadContext` or nearby workflow code executes the planner-selected context action.
- `needs_more_context()` is currently route-driven and becomes false once `PlanContext` and `ReadContext` have both appeared.
- `execute_plan_context()` treats `finish` and `ask` as terminal locally, but the outer DAG can still continue to patch generation because that decision is not persisted as workflow state.
- `NextAction.command: String` is the current bridge from planner output to action execution.

If any of these are wrong, preserve the plan intent but adjust the owning modules and names.

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

## Test Strategy

- Planner loop integration test with multiple context actions before patching.
- `AskUser` persistence and resume test (characterization here, full persistence lands in [plan_2_engine_and_persistence.md](plan_2_engine_and_persistence.md)).
- `Finish` terminal routing test.
- Typed action tests for paths with spaces and command args containing quotes.

## Open Questions

- What persisted task schema compatibility is required for existing task records?

## Handoff

Once this sub-plan's exit criteria are met, [plan_2_engine_and_persistence.md](plan_2_engine_and_persistence.md) builds the `AgentEngine` control API and durable interaction state on top of the typed action loop established here.
