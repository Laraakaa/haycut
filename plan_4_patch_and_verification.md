# Sub-Plan 4: Patch Vocabulary and First-Class Verification

Part of the [Interactive Agent Session](plan.md) milestone. Covers Phase 8 and Phase 10 of the recommended build order. Depends on `PatchProposal`/`VerificationCompleted` events from [plan_2_engine_and_persistence.md](plan_2_engine_and_persistence.md) and `FileDigest` from [plan_3_safety_and_execution.md](plan_3_safety_and_execution.md).

Shared context: both phases define what a "correct outcome" looks like once the agent decides to `PlanPatch` — expanding what edits it can make, and what checks confirm those edits worked. Both produce structured, previewable/approvable output rather than free text.

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

## Implementation Phases

### Phase 8: Expand Patch Vocabulary

- Add `Create`, `Delete`, and `Rename` edits.
- Add digest checks to all destructive or replacement operations.
- Preview all edit types before approval.
- Keep exact unique anchors for replacement edits.

Exit criteria:

- Feature tasks can create new files.
- Delete and rename operations are digest-protected.
- Patch approval displays all file operations clearly.

### Phase 10: First-Class Verification

- Store structured verification plans.
- Make `task start --verify` populate verification checks.
- Allow `verify <command>` in the REPL to add or run checks.
- Support required, optional, and changed-files-only checks.

Exit criteria:

- User verification commands override or augment project defaults.
- Verification results are persisted and emitted as `AgentEvent::VerificationCompleted`.
- Patch approval/final report can distinguish required and optional check outcomes.

## Test Strategy

- Patch vocabulary tests for replace/create/delete/rename.

## Dogfooding Criteria

- Patches require approval.
- At least one structured verification check runs after edits.

## Open Questions

- Should patch approval happen before generation, before application, or both?
