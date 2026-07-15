# HayCut Context Architecture: Phases 1-3

## Purpose

This plan covers the first three implementation phases of the HayCut context
architecture:

1. introduce versioned workflow contracts without changing behavior;
2. add ordered request assembly and request manifests in observation mode; and
3. build the dependency-aware context compiler behind a shadow mode.

The goal is to establish stable contracts and trustworthy measurements before
changing how tasks are routed or how context is selected.

## Working-tree policy

The current working tree is the baseline for this work. It already contains
substantial code-graph, symbol extraction, context-selection, eval, storage, and
view changes. Implementation must:

- keep all staged and unstaged changes;
- avoid reverting or rewriting unrelated hunks;
- build on the current `CodeGraph`, `AvailableContext`, and context-ranking
  behavior;
- prefer new modules and additive edits where that reduces merge conflicts;
- inspect the current diff before editing a file that is already modified; and
- keep the existing context-selection eval case as part of every phase gate.

The architecture work should begin only after the current working tree passes
its existing checks, or after any known failures are recorded as the baseline.

## Current foundations

The following existing components should be adapted rather than replaced:

| Foundation | Current location | Planned role |
| --- | --- | --- |
| Runtime DAG | `src/commands/agent/workflow.rs` | Execution form for compiled workflow specifications |
| Executor dispatch | `src/commands/agent.rs` | Initial primitive implementations |
| Durable task state | `src/commands/task.rs` | Compatibility aggregate for workflow and context state |
| Purpose and tier routing | `src/model.rs` | Model policy input for primitive contracts |
| Provider abstraction | `src/model.rs` | Final boundary after request assembly |
| Agent traces | `src/store.rs` | Human-readable call record linked to manifests |
| Evidence packets | `src/evidence.rs` | Typed failure context artifact |
| Compact packets | `src/compactor.rs` | Compressed command-output artifact |
| File inventory | `src/store.rs`, index command | Repository dependency source |
| Symbols and windows | `src/symbols.rs`, read commands | Selective context artifact sources |
| Code graph | `src/code_graph.rs` | Dependency discovery and context candidate source |
| Eval reports | `src/commands/eval.rs` | Behavioral and context architecture acceptance harness |

## Architectural decisions for phases 1-3

### Preserve the current runtime

`Workflow`, `Node`, and `NodeOp` remain the executable representation through
phase 3. A compiled `WorkflowSpec` will instantiate the current DAG rather than
introducing a second execution engine.

### Use stable serialized identifiers

Primitive, phase, prompt, tool-profile, and schema identifiers must have stable
string representations. Versions are explicit integers or semantic versions;
they are not inferred from Rust type names or Git revisions.

### Use reproducible digests

Persistent request and context identities must not use `DefaultHasher`, whose
output is not a suitable cross-version storage contract. Add one deterministic
digest implementation, preferably BLAKE3, and use a domain prefix in every
digest input:

```text
haycut:<artifact-kind>:<schema-version>\0<canonical-bytes>
```

Canonical JSON must use structs and deterministic map ordering. Do not hash
debug output.

### Keep manifests separate from traces

An agent trace answers "what did the model and agent do?" A request manifest
answers "what exactly was assembled and why?" Existing trace fields remain for
backward compatibility, with a nullable `manifest_id` link added later.

### Observe before selecting

Phase 2 records the current request structure without changing provider-visible
bytes. Phase 3 compiles candidate context in parallel with the legacy path.
Compiler output is not authoritative until its parity checks pass.

### Do not persist reusable context yet

Phase 3 defines artifact identity, provenance, dependencies, and freshness, but
the compiler may operate in memory. Persistent cross-request artifact reuse and
garbage collection belong to a later phase. Request manifests may persist the
artifact metadata needed to evaluate phase 3.

## Proposed module layout

The exact split may be adjusted to match implementation pressure, but the
responsibilities should remain separate:

```text
src/
  context/
    mod.rs
    artifact.rs       # ContextArtifact, dependency and provenance contracts
    compiler.rs       # Requirements, resolution, ordering, budget selection
    adapters.rs       # Existing TaskState/evidence/code-graph adapters
    comparison.rs     # Legacy-versus-compiled shadow report
    digest.rs         # Reproducible domain-separated digests
    request.rs        # ContextSegment and request assembler
  commands/
    agent/
      primitive.rs    # Primitive registry and stable contracts
      workflow_spec.rs
      workflow.rs     # Existing runtime DAG plus compatibility adapter
```

Register `context` in `src/main.rs`. Keep executor bodies in
`src/commands/agent.rs` during these phases to avoid combining architecture
changes with a large code move.

---

# Phase 1: Versioned workflow contracts

## Objective

Describe the existing workflow through stable, versioned contracts while
preserving the current routes, prompts, tools, persistence, and execution
behavior.

## 1.1 Add stable contract types

Create the following core types in `src/commands/agent/primitive.rs`:

- `PrimitiveId`
- `PrimitiveVersion`
- `PhaseId`
- `ToolProfileId`
- `ToolProfileVersion`
- `PromptId`
- `PromptVersion`
- `OutputSchemaId`
- `OutputSchemaVersion`
- `ContextCategory`
- `SideEffectPolicy`
- `RetryPolicy`
- `PrimitiveSpec`

`PrimitiveSpec` should contain:

```text
id
version
phase
executor kind
required context categories
optional context categories
tool profile id and version
prompt id and version, when applicable
output schema id and version
side-effect policy
retry policy
```

Initial phase assignments:

| `NodeOp` | Primitive ID | Phase |
| --- | --- | --- |
| `ClassifyIntent` | `classify_intent` | `intake` |
| `DetectProject` | `detect_project` | `investigation` |
| `ResolveVerification` | `resolve_verification` | `investigation` |
| `RunBaseline` | `run_baseline` | `investigation` |
| `ExtractEvidence` | `extract_evidence` | `investigation` |
| `SelectContext` | `select_context` | `investigation` |
| `PlanContext` | `plan_context` | `planning` |
| `ReadContext` | `read_context` | `investigation` |
| `PlanPatch` | `plan_patch` | `planning` |
| `ApplyPatch` | `apply_patch` | `implementation` |
| `RunFinalVerification` | `run_final_verification` | `verification` |
| `RetryFix` | `retry_fix` | `verification` |
| `AskUser` | `ask_user` | `interaction` |
| `DirectAnswer` | `direct_answer` | `reporting` |
| `Report` | `report` | `reporting` |

This phase assignment is descriptive. It must not alter route decisions yet.

## 1.2 Add the primitive registry

Add a static registry that:

- returns exactly one `PrimitiveSpec` for every `NodeOp`;
- rejects duplicate `(PrimitiveId, PrimitiveVersion)` pairs in tests;
- exposes lookup by `NodeOp` and stable primitive ID;
- preserves the existing `NodeOp::executor()` mapping;
- owns tool-profile and prompt-version associations; and
- contains no executor function pointers yet.

Keep `execute_step()` as the runtime dispatcher. It should resolve the
primitive first, assert that its executor kind matches the current dispatch,
and then execute the existing match arm.

This provides runtime coverage of the registry without moving or generalizing
the executor implementations prematurely.

## 1.3 Version current tool profiles

Replace ad hoc tool menus with named profiles while keeping the generated JSON
schemas byte-for-byte equivalent:

| Tool profile | Current source |
| --- | --- |
| `intent_classifier/v1` | `classifier_tools()` |
| `context_ranker/v1` | `relevance_tools()` |
| `context_planner/v1` | `planner_tools(task)` |
| `patch_editor/v1` | `edit_tools()` |
| `no_tools/v1` | Plain completion calls |

The planner profile is conditional because `pull` is only available when
`TaskState.available_context` is populated. Represent this as a stable base
profile plus a deterministic capability flag, not as a new profile version on
every request.

Add tests that compare the old and profile-generated `ToolDefinition` values.

## 1.4 Version current prompts

Give every model call a prompt identity even when the prompt is currently
inline:

| Model purpose | Prompt ID |
| --- | --- |
| `IntentClassification` | `intent_classification/v1` |
| `ContextRanking` | `context_ranking/v1` |
| `AgentPlanner` | `context_planner/v1` |
| `PatchGeneration` | `patch_generation/v1` |
| `FinalReport` | `direct_answer/v1` |

Keep `src/prompts/planner_system.txt` and
`src/prompts/planner_user.jinja` unchanged during initial wiring. Prompt
versioning identifies their current behavior; it does not justify prompt edits.

Move inline prompt builders only when doing so is a mechanical extraction with
golden-string tests.

## 1.5 Introduce `WorkflowSpec`

Create a serializable specification separate from runtime node status:

```text
WorkflowSpec
  schema_version
  compiler_version
  entrypoints
  nodes: WorkflowNodeSpec[]

WorkflowNodeSpec
  id
  primitive_id
  primitive_version
  dependencies
  guard
```

Implement a compatibility compiler that uses the existing `TaskIntent` and
`IntentPolicy` rules to produce the same operation sequences as the current
workflow.

Implement a `WorkflowSpec -> Workflow` adapter. Do not replace dynamic retry
decisions yet. The adapter can instantiate the deterministic prefix and use the
existing decision logic for runtime continuations.

Add an optional `workflow_spec` field to `TaskState` only after its serialization
compatibility test passes:

- use `#[serde(default, skip_serializing_if = "Option::is_none")]`;
- do not increment or reinterpret existing node IDs;
- old task JSON must load with `workflow_spec = None`; and
- newly created tasks may persist the compatibility-compiled specification.

## 1.6 Phase 1 tests

Add or extend tests for:

- every `NodeOp` resolving to one primitive;
- unique primitive, prompt, tool-profile, and output-schema identities;
- registry executor kinds matching `NodeOp::executor()`;
- tool definitions remaining equivalent;
- all four task intents compiling to expected routes;
- compiled route parity with current workflow tests;
- old `TaskState` JSON loading without a workflow specification;
- new `TaskState` JSON round-tripping the specification;
- current retry, loop detection, and budget behavior remaining unchanged; and
- the dirty-tree context-selection eval retaining its expected route and
  relevant-path behavior.

## Phase 1 deliverables

- versioned primitive contracts;
- primitive registry;
- named and versioned prompt/tool profiles;
- compatibility `WorkflowSpec`;
- runtime registry coverage; and
- behavior and serialization parity tests.

## Phase 1 exit gate

- `cargo test` passes;
- `cargo clippy --all-targets --all-features` passes;
- existing eval cases have unchanged pass/fail verdicts;
- every executed node reports a primitive ID and version;
- old task JSON remains readable; and
- no provider-visible prompt or tool-schema changes are introduced.

---

# Phase 2: Ordered request assembly and manifests

## Objective

Make every model request explainable and measurable without changing the
provider-visible request content.

## 2.1 Add context-segment contracts

Create `ContextSegment` in `src/context/request.rs`:

```text
id
position
role
category
representation
schema version
producer ID and version
content
content digest
provenance
dependency digests
byte size
estimated tokens
cache policy
```

Initial roles:

- `system`
- `tool_definition`
- `instruction`
- `repository`
- `task`
- `checkpoint`
- `context`
- `evidence`
- `recent_output`

Initial representations:

- `raw`
- `extracted`
- `compressed`
- `generated`

In phase 2, segments may be coarse. For example, the complete current planner
user prompt can initially be one `task` segment if splitting it would alter
bytes. Finer categories are introduced in phase 3.

## 2.2 Add a request assembler

Add an assembler that accepts:

- the resolved `PrimitiveSpec`;
- ordered system and user segments;
- the resolved tool profile;
- `ModelPurpose`;
- model-output limit;
- reasoning effort; and
- task/node correlation data.

It returns:

```text
AssembledRequest
  ModelRequest
  RequestManifestDraft
```

The assembler is the only production path that may construct a `ModelRequest`.
Migrate current call sites incrementally:

1. `model_request()` used by planning and patch generation;
2. direct answer;
3. intent classification; and
4. context ranking.

After migration, tests may still construct `ModelRequest` directly, but
production code outside the assembler should not.

For observation mode, add golden tests proving that assembled:

- system text;
- user prompt text;
- tool definitions;
- output limit;
- reasoning effort; and
- token estimate

match the legacy request for each model purpose.

## 2.3 Define request manifests

Add serializable manifest types:

```text
RequestManifest
  schema_version
  id
  task_id
  step_index
  node_id
  workflow/compiler version
  primitive ID/version
  phase
  model
  purpose
  prompt ID/version
  tool-profile ID/version
  reasoning effort
  ordered segment descriptors
  request digest
  status
  estimated usage
  reported usage
  cached input
  provider request ID
  latency
  billing flag
  error summary
  timestamps
```

Segment descriptors contain hashes and metadata, not a second mandatory copy of
the prompt body. Existing `agent_traces.prompt` and `response` remain the raw
inspection surface under the current storage policy.

Manifest statuses:

- `prepared`
- `completed`
- `provider_failed`
- `recording_failed`

Do not represent an incomplete or failed request as completed.

## 2.4 Add ordered SQLite migrations

Increase the store schema from version 5 using an explicit migration step.
Keep the existing "reject newer schema" behavior.

Add:

```sql
request_manifests
request_manifest_segments
```

Add nullable `manifest_id` to `agent_traces`.

Recommended indexes:

- manifests by `(task_id, step_index)`;
- manifests by `request_digest`;
- manifests by `(primitive_id, primitive_version)`;
- segments by `(manifest_id, position)`; and
- traces by `manifest_id`.

Persist the manifest and all segment descriptors in one transaction before the
provider call, with status `prepared`. Update usage, provider metadata, status,
latency, and error information after the call.

Failure policy:

- if the prepared manifest cannot be stored, do not send the model request;
- if the provider fails, update the manifest to `provider_failed` and preserve
  existing executor fallback behavior;
- if final manifest update fails after a provider response, return an explicit
  recording error and leave a detectable incomplete manifest;
- never automatically repeat a billed model call solely because recording its
  response failed.

Add migration tests for:

- creating a fresh database;
- upgrading a version-5 database;
- loading existing traces with `manifest_id = NULL`;
- manifest/segment transaction rollback;
- ordered segment retrieval; and
- rejecting a database newer than the supported version.

## 2.5 Centralize model invocation accounting

Wrap both provider paths:

- plain completion; and
- completion with tools.

The invocation wrapper should:

1. persist the prepared manifest;
2. capture monotonic start time;
3. call the existing `ModelProvider`;
4. capture reported input, output, and cached-input tokens;
5. copy safe provider metadata such as request ID;
6. finalize the manifest;
7. write the existing agent trace with `manifest_id`; and
8. return the existing response/tool-call shape to the executor.

Do not move provider-specific cache controls into the generic manifest. Record a
provider-neutral cache policy only; provider translation belongs to a later
phase.

## 2.6 Expose manifest data in eval reports

Extend `EvalEvidence` to load manifests for the task. Increment the eval report
schema version and add:

```text
request_summary
  request count
  prepared/incomplete/failed counts
  segment count
  estimated input/output
  reported input/output
  reported cached input
  fresh input
  cache ratio when available
  latency total and percentiles when available

requests[]
  manifest identity
  primitive and phase
  request digest
  ordered segment descriptors
  usage and outcome
```

Keep current `model_usage` and token summaries during this phase. New metrics
must be reconciled against them before any old field is removed.

Add a report check that fails an eval when a model call has no completed or
explicitly failed manifest.

## Phase 2 deliverables

- one request assembler;
- reproducible request/segment digests;
- durable manifests and ordered segments;
- agent-trace links;
- latency and provider cache accounting; and
- eval report coverage.

## Phase 2 exit gate

- all production model calls use the assembler and invocation wrapper;
- every attempted model call has a manifest;
- current prompt and tool bytes remain equivalent;
- manifest usage reconciles with `ModelLedger` and agent traces;
- version-5 databases upgrade without data loss;
- existing eval verdicts do not regress; and
- request instrumentation can be disabled only in tests, not silently in
  production.

---

# Phase 3: Dependency-aware context compiler in shadow mode

## Objective

Compile typed context bundles from existing HayCut state and repository
artifacts, compare them with legacy prompts, and cut over only low-risk
primitives after parity is demonstrated.

## 3.1 Define context requirements

Add `ContextRequirement`:

```text
category
required or optional
minimum and maximum cardinality
allowed representations
freshness policy
maximum tokens
priority
ordering group
```

Initial categories:

- `task_goal`
- `acceptance_criteria`
- `constraints`
- `project_environment`
- `verification_plan`
- `current_failure`
- `failure_evidence`
- `observations`
- `hypotheses`
- `repository_inventory`
- `relevant_symbol`
- `relevant_window`
- `code_graph_candidate`
- `recent_tool_output`
- `patch_plan`
- `current_changes`

Attach requirements to `PrimitiveSpec`. They are descriptive in shadow mode and
authoritative only for primitives explicitly enabled for compiled context.

## 3.2 Define context artifacts and dependencies

Use `ContextArtifact` in code to match the repository's existing `Artifact`
terminology:

```text
artifact ID
category
representation
schema version
producer ID/version
content or content reference
content digest
provenance
dependency list
repository/worktree identity
freshness result
byte and token size
```

Initial dependency kinds:

- file path plus content hash;
- stored run ID plus evidence/compact digest;
- task ID plus relevant field digest;
- code-graph build inputs;
- producer and schema version;
- configuration digest; and
- tool-profile or prompt version when generated content depends on it.

Freshness must be deterministic:

- a file-backed artifact is fresh only when its current content hash matches;
- a run-backed artifact is fresh only when the stored packet digest matches;
- a task-backed artifact is fresh only when the relevant field digest matches;
- a generated artifact is stale when its producer or schema version changes;
- missing dependencies make required artifacts invalid, not silently fresh.

## 3.3 Adapt existing context sources

Add adapters rather than new retrieval mechanisms:

| Existing source | Artifact output |
| --- | --- |
| `TaskState.goal`, acceptance, constraints | Task requirement artifacts |
| `ProjectCard`, `VerificationPlan` | Environment and verification artifacts |
| `CurrentFailure` | Current-failure artifact |
| `Observation`, `Hypothesis` | Extracted reasoning-state artifacts |
| `EvidencePacket` | Failure-evidence artifact |
| `CompactPacket` | Compressed command-output artifact |
| file inventory | Repository-inventory artifacts |
| symbol extraction | Relevant-symbol artifact |
| file windows | Relevant-window artifact |
| `CodeGraph` candidates | Candidate metadata artifacts |
| `AvailableContext` | Lazily selectable symbol artifacts |

The compiler must use the current code-graph and weak relevance result. It must
not independently repeat model ranking in shadow mode.

## 3.4 Implement deterministic compilation

The compiler takes:

```text
TaskState
PrimitiveSpec
repository state
available source adapters
context budget
```

Resolution order:

1. required policy and task-contract artifacts;
2. exact fresh artifacts already available in task/run state;
3. deterministic selective retrieval;
4. existing extracted representations;
5. existing compressed representations; and
6. omission with an explicit reason.

For phase 3:

- do not perform unbounded whole-repository reads;
- do not invoke a model from the compiler;
- do not compress merely to hit a smaller prompt;
- do not include stale artifacts;
- preserve a stable category and artifact ordering; and
- fail compilation when a required category cannot be resolved.

Return:

```text
CompiledContext
  compiler version
  primitive identity
  selected artifacts
  omitted candidates and reasons
  unresolved requirements
  total bytes/tokens
  bundle digest
```

## 3.5 Add shadow mode

Add a backward-compatible configuration:

```toml
[context]
compiler_mode = "off" # off | shadow | on
```

Default to `off` when the field is absent. Enable `shadow` in dedicated tests
and eval runs before considering a default change.

In shadow mode:

1. build the legacy request exactly as phase 2 does;
2. compile the candidate context bundle;
3. do not replace any legacy segment;
4. compare legacy and compiled context;
5. attach comparison results to the request manifest; and
6. continue the legacy call even if optional compiler output differs.

A missing required artifact, stale selected artifact, compiler error, or
dependency error must be visible in the manifest and eval report. It must not be
reduced to an empty successful comparison.

## 3.6 Define parity comparisons

Add `ContextCompilationComparison` with:

- required categories present/missing;
- source identities present only in legacy or compiled context;
- selected artifact digests;
- stale artifact count;
- duplicate digest count;
- category-order violations;
- legacy and compiled token totals;
- unresolved requirements;
- compiler duration; and
- verdict with explicit reasons.

Do not require byte equality between a monolithic legacy prompt and a structured
bundle. Require semantic source coverage for required categories and exact
digest equality where both paths use the same source body.

## 3.7 Cut over low-risk primitives

Support per-primitive compiled-context enablement so rollout does not require a
global switch.

Cutover order:

1. intent classification: task goal only;
2. context ranking: failure evidence and code-graph candidates;
3. direct answer;
4. context planning; and
5. patch generation.

For each primitive:

- run shadow mode across unit tests and applicable eval cases;
- resolve every required-category mismatch;
- add a golden assembled-request test;
- enable compiled mode only for that primitive;
- compare eval verdicts, route, token use, and context coverage; and
- retain the legacy builder as rollback until all phase-3 gates pass.

Planning and patch generation should not cut over in the same change.

## 3.8 Phase 3 tests

Add tests for:

- stable artifact and bundle digests;
- file mutation invalidating symbol/window artifacts;
- evidence changes invalidating failure artifacts;
- producer-version changes invalidating generated artifacts;
- missing dependencies failing required resolution;
- duplicate artifact removal;
- stable artifact ordering;
- budget selection preserving required artifacts;
- raw, extracted, and compressed representation preference;
- shadow mode leaving provider-visible requests unchanged;
- compiler failures appearing in manifests;
- per-primitive cutover and rollback; and
- the distractor eval selecting relevant code-graph context without injecting
  irrelevant symbols.

## Phase 3 deliverables

- typed context requirements and artifacts;
- deterministic dependency and freshness checks;
- adapters for current HayCut context sources;
- an in-memory context compiler;
- shadow comparisons in manifests and eval reports; and
- low-risk per-primitive cutover support.

## Phase 3 exit gate

- shadow mode never changes legacy model requests;
- no stale artifact passes freshness tests;
- all required-category mismatches are explicit;
- compiled intent classification and context ranking pass their evals;
- planning and patch generation show context parity before cutover;
- existing task, trace, and database compatibility tests pass; and
- the legacy context path remains selectable for rollback.

---

# Merge-sized implementation sequence

Use small changes that can be tested independently:

1. Add stable IDs, versions, phases, and registry tests.
2. Register all current `NodeOp` values and assert executor parity.
3. Add versioned prompt and tool-profile descriptors with golden tests.
4. Add `WorkflowSpec`, compatibility compilation, and task-state compatibility.
5. Add digest and `ContextSegment` primitives.
6. Add the assembler and migrate one model purpose at a time.
7. Add the version-6 manifest migration and store APIs.
8. Add the invocation wrapper and link agent traces.
9. Add manifest fields to eval reports and reconciliation checks.
10. Add context requirements, artifacts, and existing-source adapters.
11. Add deterministic compilation and dependency invalidation tests.
12. Add shadow comparisons to manifests and eval reports.
13. Cut over intent classification.
14. Cut over context ranking.
15. Evaluate direct-answer, planning, and patch-generation cutovers separately.

Each item should leave `cargo test` passing. Avoid combining schema migration,
all call-site migration, and context cutover in one change.

# Validation matrix

Run at minimum after every merge-sized item:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features
```

At each phase gate, run every eval case:

```sh
cargo run -- eval list
cargo run -- eval run sum_wrong_assertion_py
cargo run -- eval run sum_wrong_assertion_rs
cargo run -- eval run split_context_off_by_one_rs
cargo run -- eval run select_context_ignores_distractors_rs
```

Also retain fixtures for:

- loading a version-5 database;
- loading task JSON without new optional fields;
- a provider response with cached-input usage;
- a provider failure after manifest preparation;
- a dirty worktree changing one artifact dependency; and
- identical requests producing identical request and segment digests.

# Completion criteria for phases 1-3

Phases 1-3 are complete when:

- current workflows are represented by versioned primitive contracts;
- current behavior can be reproduced from a compatibility `WorkflowSpec`;
- every model request is assembled through one path;
- every attempted call has a durable, ordered request manifest;
- manifest usage reconciles with traces and provider reporting;
- existing context sources compile into typed, dependency-aware artifacts;
- shadow mode reports semantic parity without altering requests;
- at least intent classification and context ranking use compiled context;
- all existing eval verdicts are preserved; and
- old databases and task JSON remain readable.

At that point HayCut will have the stable contracts, observability, and context
compiler needed for dynamic task compilation and phase-level cache domains
without having paid the risk of a runtime rewrite.
