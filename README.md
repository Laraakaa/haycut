# HayCut

Less context. More signal.

HayCut is an open-source coding harness for building token-efficient AI development workflows.

Most coding agents spend too much context before they know what matters. They read broad files, repeat noisy commands, dump logs into prompts, and ask expensive models to do work that deterministic tools could have handled.

HayCut takes the opposite approach:

> Cut down the haystack before looking for the needle.

The goal is not just to compress context. The goal is to avoid useless context in the first place.

## Vision

HayCut treats tokens as a first-class engineering resource.

Every file read, tool call, test run, model request, log summary, patch and verification step should justify its cost. Context should be included because it is likely to change the outcome, not because it might be vaguely useful.

HayCut is designed around a simple principle:

> Less tokens by design. Not less correctness.

A cheap broken patch is not efficient. HayCut optimises for verified useful work per token.

## What HayCut does

HayCut provides a harness around coding agents and model workflows with:

- **Token budgets**:
    Set soft and hard token budgets per task.
- **Token tracing**:
    See where tokens were spent across model calls, file reads, command output, retries and verification.
- **Repo-aware context selection**:
    Prefer symbols, call graphs, test relationships and targeted file windows over whole-file dumps.
- **Structured evidence packets**:
    Send the model compact, relevant context instead of raw haystacks.
- **Command output gating**:
    Summarise test failures, stack traces and logs before they reach the model.
- **Loop detection**
    Stop repeated file reads, repeated failed test runs and context expansion without new information.
- **Model routing**:
    Use cheap or local models for triage, summarisation and ranking; reserve stronger models for reasoning and patch decisions.
- **Verification-first reporting**:
    Measure token savings only alongside task success, tests passed and patch quality.

## Design Principles

- No context without a reason.
- No whole-file read when a symbol or file window would do.
- No repeated command without changed state.
- No expensive model call for deterministic work.
- No prose when a diff, test result or trace is more useful.
- No compression that hides important source details.
- No token-saving claim without correctness metrics.
- No "cheap" result unless it is actually verified.

## Status

HayCut is early-stage.

The intended first milestone is a CLI harness that can:

- run a coding task with a token budget;
- trace token usage;
- index a repository;
- provide targeted context tools;
- gate command output;
- produce a report showing tokens, cost, retries, verification and waste.

## Name

HayCut comes from the idea of cutting down the haystack before searching for the needle.

It is not just about finding better context.

It is about destroying irrelevant context before it reaches the model.
