# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What HayCut is

HayCut is a token-efficient coding harness (Rust CLI, binary-only crate). Its thesis: "cut down the haystack before looking for the needle" — avoid useless model context in the first place rather than compressing it after the fact. It runs coding tasks under token budgets, indexes the repo, selects targeted context (symbols/windows/call-graph over whole-file dumps), gates command output before it reaches the model, and reports token spend alongside correctness metrics.

## Build / test / run

Binary-only crate — there is **no lib target**. Always target the binary:

```bash
cargo build --bin haycut          # NOT --lib (fails: no lib target)
cargo test  --bin haycut          # run all tests
cargo test  --bin haycut <substr> # ONE filter only; `cargo test A B` is invalid
cargo check                       # fast type check (also `make check`)
cargo clippy --all-targets --all-features   # or `make clippy`
cargo fmt                         # or `make fmt`
cargo run -- <subcommand> ...     # or `make run ARGS="..."`
```

- `timeout` is not installed (macOS); run commands directly.
- `cargo-insta` is not installed. To accept snapshot changes, `find . -name "*.snap.new"` and `mv` each over its `.snap`.
- The shell `grep` is ripgrep (rtk-backed): unescaped `(`, `{`, `|`, `\b(` in patterns cause "regex parse error" / "unclosed group". Use plain symbol names without trailing `(` or `{`, or `grep -F`. Quote `--include="*.rs"`.

## CLI surface (`src/cli.rs`)

All subcommands are declared in `src/cli.rs` (`Commands` enum) and dispatched to `commands::<name>::run`. Key ones:

- `init` — create `.haycut/` store in the repo.
- `index` / `files` / `search` / `read-symbol` / `read-window` — deterministic repo context tools.
- `trace <cmd>` — run a command, capture + compact its output into a run.
- `report` / `packet` / `runs` — inspect captured runs; build evidence packets under a budget.
- `task start|status|list|close` — durable per-repo task state (goal + verify command).
- `agent run|step|session|trace` — the constrained agent loop (see below).
- `eval list|run <case>` — run gold-set eval cases (`evals/cases/`).
- `view --port <p>` — local dashboard serving eval/agent runs from `evals/results/`.

## Architecture

**Agent workflow is a graph, not an ad-hoc loop.** `src/commands/agent/workflow.rs` defines `NodeOp` — the concrete step kinds (ClassifyIntent, DetectProject, ResolveVerification, RunBaseline, ExtractEvidence, SelectContext, PlanContext, ReadContext, PlanPatch, ApplyPatch, RunFinalVerification, RetryFix, AskUser, DirectAnswer, Report). Each `NodeOp` maps 1:1 onto an `execute_*` function in `src/commands/agent.rs` — new agent behavior means a new `execute_*` plus wiring a node, not a new bespoke loop.

**Engine is the single control contract.** `src/commands/agent/engine.rs` exposes `AgentEvent`s out / `ControlCommand`s in. The CLI, terminal REPL (`session.rs`), and dashboard all drive the agent through this one API — do not re-implement the step/decide loop in callers. `AskUser`/approval flows go through `PendingInteraction` / `ApprovalRequest`.

**Model routing by tier.** `src/model.rs` + `src/config.rs` split work across a weak (cheap/local) and strong model tier (`[weak_model]` / `[strong_model]`, falling back to `[model]`). Weak tier handles classification/selection/summarization; strong tier handles context and patch planning. Providers are OpenAI-compatible; a local proxy at `localhost:4000` or Ollama at `localhost:11434/v1` is typical for dev.

**Context pipeline** (`src/context/`): `request.rs` → `compiler.rs` → `artifact.rs` compile structured context requests into compact artifacts; `code_graph.rs`, `symbols.rs`, `extract.rs`, `evidence.rs`, `compactor.rs` provide the repo-aware primitives (call graph, tree-sitter symbol parsing for Rust/Python/TypeScript, failure extraction, output compaction).

**Persistence** (`src/store.rs`): SQLite at `.haycut/haycut.sqlite3`. Tables: `runs`, `artifacts`, `files`, `tasks`, `settings`, `agent_traces`, `request_manifests(_segments)`.

## Schema migrations — do all three in the SAME change

Adding a column to `agent_traces` (or any stored struct) requires, together:
1. Bump `SCHEMA_VERSION` in `src/store.rs`.
2. Add the `ALTER`/migration in `src/store.rs::migrate`.
3. Update the report.json (de)serializer in `src/commands/view/model.rs` and `src/commands/eval.rs`.

Skipping (1)+(2) → `table agent_traces has no column named X` at runtime. Skipping (3) → `haycut view` drops runs with a "missing field" error.

## Evals

Cases live in `evals/cases/<name>/` (`case.toml` + a `repo/` fixture); names carry a `_rs`/`_py` suffix (e.g. `split_context_off_by_one_rs`, `sum_wrong_assertion_py`) — there is no unsuffixed variant. Results land in `evals/results/<TIMESTAMP>-<case>/`.

`cargo run -- eval run <case>` needs a working model proxy — verify before looping:
```bash
curl -s http://localhost:4000/health   # or: curl -s http://localhost:11434/api/version
```
If down, the run fails with "model API returned no tool call". Clean stale results with `rm -rf evals/results/2026*` before re-running.

## Large hot files — read once, edit in batches

`src/commands/agent.rs` is ~2600 lines / ~115KB and is the most-edited file by far. Read it (or `grep -n` to map function line ranges) once at task start, keep it in context, and apply consecutive edits without re-reading between them. Same discipline for `src/store.rs`, `src/model.rs`, `src/commands/eval.rs`, `src/commands/task.rs`. The Read `offset` parameter must be a **number**, not a string.
