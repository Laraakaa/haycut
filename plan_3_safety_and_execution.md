# Sub-Plan 3: Command Approval Policy and Working-Tree Ownership

Part of the [Interactive Agent Session](plan.md) milestone. Covers Phase 7 and Phase 9 of the recommended build order. Depends on the engine control API and `ApprovalRequest`/`ApprovalRequired` event from [plan_2_engine_and_persistence.md](plan_2_engine_and_persistence.md).

Shared context: both phases are execution-safety guardrails that gate agent actions against unwanted side effects — one for arbitrary commands, one for file writes — and both report failures back to the planner as recoverable observations rather than hard errors.

## Approval Model

Require approval before applying patches and before running non-trivial commands.

Command risk policy:

| Risk | Default behavior | Examples |
| --- | --- | --- |
| Low | auto-allow | tests, build, lint, formatting checks, read-only Git |
| Medium | ask first | package installation, code generation, migrations, commands that modify tracked files |
| High | deny by default | destructive filesystem operations, destructive Git operations, network publishing, credential or secret access |

Add command timeout and cancellation support. Every command event should record command, args, working directory, timeout, exit status, duration, stdout/stderr summary, and whether it was approved.

## Working-Tree Ownership

Move from "refuse any file with uncommitted changes" to optimistic concurrency.

Desired contract:

- Record file digest when context is read.
- Allow pre-existing user changes.
- Apply only if the current digest matches the digest used for patch planning.
- Refuse if the file changed after the agent inspected it.
- Report conflicts as recoverable observations and return to planning.
- Later, add an option for isolated temporary Git worktrees in highly autonomous mode.

## Implementation Phases

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

### Phase 9: Improve Working-Tree Ownership

- Record digests when reading context.
- Permit pre-existing dirty files if the agent has a digest for the inspected version.
- Refuse to apply if the file changed after inspection.
- Return conflicts to the planner as observations.

Exit criteria:

- User edits made before agent inspection do not block work.
- User edits made after agent inspection cause a recoverable conflict.
- Conflict behavior is covered by tests.

## Test Strategy

- Command risk policy tests.
- Digest conflict tests for optimistic concurrency.

## Dogfooding Criteria

- Non-trivial commands require approval or are denied.

## Open Questions

- Should command risk policy be configurable per repository?

## Handoff

`FileDigest` from working-tree ownership is a precondition for the digest-protected `Delete`/`Rename` edits in [plan_4_patch_and_verification.md](plan_4_patch_and_verification.md); land Phase 9 before or alongside Phase 8.
