use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::commands::task::{CurrentFailure, TaskIntent, TaskState};

/// A single decision in the agent task-state machine.
/// `TaskState + Capabilities + Policy -> NextStep`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextStep {
    /// Cheap weak-model intent classification.
    ClassifyIntent,
    /// Deterministic project-environment detection from marker files.
    DetectProject,
    /// Deterministic verification command resolution from the project card.
    ResolveVerification,
    /// Run the verification command to establish a baseline.
    RunBaseline,
    /// Extract a compact failure observation from the baseline run.
    ExtractEvidence,
    /// Strong-model planning of what context to read next.
    PlanContext,
    /// Deterministic read of a symbol, window, or search result.
    ReadContext,
    /// Strong-model patch planning.
    PlanPatch,
    /// Apply the planned patch to the working tree.
    ApplyPatch,
    /// Run the verification command to confirm the fix.
    RunFinalVerification,
    /// Loop back to context/plan after a failed verification.
    RetryFix,
    /// Ask the user a clarifying question.
    AskUser,
    /// Answer the user's question directly (no repo mutation).
    DirectAnswer,
    /// Produce the final report.
    Report,
    /// Terminal state.
    Stop(StopReason),
}

/// Why the agent stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Verification passed.
    Verified,
    /// The same failure signature appeared twice without the patch changing.
    LoopDetected,
    /// The hard token budget is exhausted.
    BudgetExhausted,
    /// Progress is blocked and the agent cannot continue autonomously.
    Blocked,
    /// A deterministic step failed irrecoverably.
    Failed,
    /// The step limit was reached.
    MaxSteps,
}

/// What kind of executor should run a step.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    /// No model call: pure code/rules.
    Deterministic,
    /// Cheap model: intent classification, ranking, summarisation.
    WeakModel,
    /// Capable model: planning, patch generation, final report.
    StrongModel,
    /// External process: test/build commands.
    Command,
}

impl NextStep {
    /// Executor kind that should run this step.
    pub fn executor(&self) -> ExecutorKind {
        match self {
            NextStep::ClassifyIntent => ExecutorKind::WeakModel,
            NextStep::DetectProject => ExecutorKind::Deterministic,
            NextStep::ResolveVerification => ExecutorKind::Deterministic,
            NextStep::RunBaseline => ExecutorKind::Command,
            NextStep::ExtractEvidence => ExecutorKind::Deterministic,
            NextStep::PlanContext => ExecutorKind::StrongModel,
            NextStep::ReadContext => ExecutorKind::Deterministic,
            NextStep::PlanPatch => ExecutorKind::StrongModel,
            NextStep::ApplyPatch => ExecutorKind::Deterministic,
            NextStep::RunFinalVerification => ExecutorKind::Command,
            NextStep::RetryFix => ExecutorKind::Deterministic,
            NextStep::AskUser => ExecutorKind::Deterministic,
            NextStep::DirectAnswer => ExecutorKind::StrongModel,
            NextStep::Report => ExecutorKind::StrongModel,
            NextStep::Stop(_) => ExecutorKind::Deterministic,
        }
    }

    /// Short snake_case name used in route logging and tests.
    pub fn name(&self) -> &'static str {
        match self {
            NextStep::ClassifyIntent => "classify_intent",
            NextStep::DetectProject => "detect_project",
            NextStep::ResolveVerification => "resolve_verification",
            NextStep::RunBaseline => "run_baseline",
            NextStep::ExtractEvidence => "extract_evidence",
            NextStep::PlanContext => "plan_context",
            NextStep::ReadContext => "read_context",
            NextStep::PlanPatch => "plan_patch",
            NextStep::ApplyPatch => "apply_patch",
            NextStep::RunFinalVerification => "run_final_verification",
            NextStep::RetryFix => "retry_fix",
            NextStep::AskUser => "ask_user",
            NextStep::DirectAnswer => "direct_answer",
            NextStep::Report => "report",
            NextStep::Stop(_) => "stop",
        }
    }
}

impl ExecutorKind {
    /// Short snake_case name used in trace output.
    pub fn name(&self) -> &'static str {
        match self {
            ExecutorKind::Deterministic => "det",
            ExecutorKind::WeakModel => "weak",
            ExecutorKind::StrongModel => "strong",
            ExecutorKind::Command => "cmd",
        }
    }
}

/// Per-intent policy flags controlling the state-machine route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntentPolicy {
    pub needs_repo: bool,
    pub requires_verification: bool,
    pub expect_baseline_failure: bool,
    pub patch_expected: bool,
    pub require_final_verification: bool,
}

impl IntentPolicy {
    pub fn for_intent(intent: TaskIntent) -> Self {
        match intent {
            TaskIntent::DebugFailure => Self {
                needs_repo: true,
                requires_verification: true,
                expect_baseline_failure: true,
                patch_expected: true,
                require_final_verification: true,
            },
            TaskIntent::ImplementFeature => Self {
                needs_repo: true,
                requires_verification: true,
                expect_baseline_failure: false,
                patch_expected: true,
                require_final_verification: true,
            },
            TaskIntent::Refactor => Self {
                needs_repo: true,
                requires_verification: true,
                expect_baseline_failure: false,
                patch_expected: true,
                require_final_verification: true,
            },
            TaskIntent::AnswerQuestion => Self {
                needs_repo: true,
                requires_verification: false,
                expect_baseline_failure: false,
                patch_expected: false,
                require_final_verification: false,
            },
        }
    }
}

/// Pure, testable policy engine: given the current task state, decide the
/// next step. No side effects, no model calls.
pub fn select_next_step(task: &TaskState, max_retries: usize) -> NextStep {
    // Hard stop: budget exhausted.
    if task.budget.packet_tokens_used >= task.budget.hard_tokens {
        return NextStep::Stop(StopReason::BudgetExhausted);
    }

    let Some(intent) = task.intent else {
        return NextStep::ClassifyIntent;
    };

    let policy = IntentPolicy::for_intent(intent);

    // Baseline verification loop: if the last final verification failed, decide
    // whether to retry, loop-detect, give up, or ask the user.
    if let Some(last_final) = task
        .route
        .iter()
        .rev()
        .find(|entry| entry.step == NextStep::RunFinalVerification.name())
    {
        let failed = !outcome_passed(&last_final.outcome);
        if failed {
            return resolve_retry(task, policy, max_retries);
        }
    }

    // Route progression.
    if task.project.is_none() {
        return NextStep::DetectProject;
    }

    if policy.requires_verification && task.verification.is_none() {
        return NextStep::ResolveVerification;
    }

    if policy.requires_verification && !has_step(task, NextStep::RunBaseline.name()) {
        return NextStep::RunBaseline;
    }

    if policy.expect_baseline_failure
        && task.current_failure.is_none()
        && !has_step(task, NextStep::ExtractEvidence.name())
    {
        return NextStep::ExtractEvidence;
    }

    // Context-gathering phase.
    if needs_more_context(task, policy) {
        if task.next_actions.is_empty() {
            return NextStep::PlanContext;
        }
        return NextStep::ReadContext;
    }

    // Patch phase.
    if policy.patch_expected {
        if task.patch_text.is_none() {
            return NextStep::PlanPatch;
        }
        if !task.patch_applied {
            return NextStep::ApplyPatch;
        }
        if policy.require_final_verification
            && !has_step(task, NextStep::RunFinalVerification.name())
        {
            return NextStep::RunFinalVerification;
        }
    }

    // Non-patch intents: direct answer.
    if !policy.patch_expected && !has_step(task, NextStep::DirectAnswer.name()) {
        return NextStep::DirectAnswer;
    }

    // Terminal report.
    if !has_step(task, NextStep::Report.name()) {
        return NextStep::Report;
    }

    // Determine the correct stop reason from history.
    let verified = policy.require_final_verification && has_passing_final_verification(task)
        || !policy.patch_expected;
    let reason = if verified {
        StopReason::Verified
    } else {
        StopReason::Blocked
    };

    NextStep::Stop(reason)
}

/// Decide what to do after a failed final verification.
fn resolve_retry(task: &TaskState, _policy: IntentPolicy, max_retries: usize) -> NextStep {
    if task.retry_count >= max_retries {
        return NextStep::Stop(StopReason::BudgetExhausted);
    }

    let current_signature = task.current_failure.as_ref().map(failure_signature);
    let previous_signature = task.last_failure_signature.clone();

    if current_signature.is_some() && current_signature == previous_signature {
        return NextStep::Stop(StopReason::LoopDetected);
    }

    if task.patch_text.is_some() {
        return NextStep::RetryFix;
    }

    if task.budget.packet_tokens_used >= task.budget.soft_tokens {
        return NextStep::AskUser;
    }

    NextStep::Stop(StopReason::Blocked)
}

/// Whether the agent still needs more context before it can plan a patch or
/// answer. Uses existing observations, hypotheses, and next_actions.
fn needs_more_context(task: &TaskState, policy: IntentPolicy) -> bool {
    if !policy.needs_repo {
        return false;
    }

    // Non-patch intents take the direct-answer path: no context gathering.
    if !policy.patch_expected {
        return false;
    }

    // If the planner already queued reads, we are still in context gathering.
    if !task.next_actions.is_empty() {
        return true;
    }

    // Patch intents: we must run the planner at least once (which may queue
    // deterministic reads) before a patch can be planned.
    if task.patch_text.is_none()
        && !has_step(task, NextStep::PlanContext.name())
        && !has_step(task, NextStep::ReadContext.name())
    {
        return true;
    }

    false
}

/// Normalised signature of a failure for loop detection.
pub fn failure_signature(failure: &CurrentFailure) -> String {
    let mut hasher = DefaultHasher::new();
    failure.kind.hash(&mut hasher);
    failure.summary.hash(&mut hasher);
    for location in &failure.locations {
        location.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// True if the route contains a passing final verification.
fn has_passing_final_verification(task: &TaskState) -> bool {
    task.route
        .iter()
        .rev()
        .find(|entry| entry.step == NextStep::RunFinalVerification.name())
        .map(|entry| outcome_passed(&entry.outcome))
        .unwrap_or(false)
}

/// Parse a command outcome string for pass/fail.
fn outcome_passed(outcome: &str) -> bool {
    outcome.trim().starts_with("pass") || outcome.trim() == "0" || outcome.contains("exit 0")
}

fn has_step(task: &TaskState, name: &str) -> bool {
    task.route.iter().any(|entry| entry.step == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::task::{
        NextAction, ProjectCard, RouteEntry, TaskBudget, VerificationPlan,
    };

    fn empty_task() -> TaskState {
        TaskState {
            schema_version: 1,
            id: "task-test".to_string(),
            title: "test".to_string(),
            goal: "test goal".to_string(),
            acceptance: vec!["cargo test passes".to_string()],
            constraints: Vec::new(),
            budget: TaskBudget {
                soft_tokens: 40_000,
                hard_tokens: 80_000,
                packet_tokens_used: 0,
                raw_tokens_avoided: 0,
            },
            runs: Vec::new(),
            observations: Vec::new(),
            hypotheses: Vec::new(),
            next_actions: Vec::new(),
            intent: None,
            current_failure: None,
            closed_at: None,
            project: None,
            verification: None,
            route: Vec::new(),
            patch_text: None,
            patch_applied: false,
            retry_count: 0,
            last_failure_signature: None,
        }
    }

    fn task_with_intent(intent: TaskIntent) -> TaskState {
        let mut task = empty_task();
        task.intent = Some(intent);
        task
    }

    fn push_route(task: &mut TaskState, step: NextStep, outcome: &str) {
        task.route.push(RouteEntry {
            step: step.name().to_string(),
            executor: step.executor(),
            outcome: outcome.to_string(),
        });
    }

    #[test]
    fn empty_task_starts_with_classify() {
        let task = empty_task();
        assert_eq!(select_next_step(&task, 2), NextStep::ClassifyIntent);
    }

    #[test]
    fn classified_task_detects_project() {
        let task = task_with_intent(TaskIntent::DebugFailure);
        assert_eq!(select_next_step(&task, 2), NextStep::DetectProject);
    }

    #[test]
    fn debug_failure_runs_baseline_after_verification_resolved() {
        let mut task = task_with_intent(TaskIntent::DebugFailure);
        task.project = Some(ProjectCard {
            language: "Rust".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
        task.verification = Some(VerificationPlan {
            command: vec!["cargo".to_string(), "test".to_string()],
            expected_baseline_exit: Some(101),
            expected_final_exit: 0,
        });
        assert_eq!(select_next_step(&task, 2), NextStep::RunBaseline);
    }

    #[test]
    fn debug_failure_extracts_evidence_after_baseline() {
        let mut task = task_with_intent(TaskIntent::DebugFailure);
        task.project = Some(ProjectCard {
            language: "Rust".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
        task.verification = Some(VerificationPlan {
            command: vec!["cargo".to_string(), "test".to_string()],
            expected_baseline_exit: Some(101),
            expected_final_exit: 0,
        });
        push_route(&mut task, NextStep::RunBaseline, "fail exit 101");
        assert_eq!(select_next_step(&task, 2), NextStep::ExtractEvidence);
    }

    #[test]
    fn debug_failure_plans_context_after_evidence() {
        let mut task = task_with_intent(TaskIntent::DebugFailure);
        task.project = Some(ProjectCard {
            language: "Rust".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
        task.verification = Some(VerificationPlan {
            command: vec!["cargo".to_string(), "test".to_string()],
            expected_baseline_exit: Some(101),
            expected_final_exit: 0,
        });
        push_route(&mut task, NextStep::RunBaseline, "fail exit 101");
        push_route(
            &mut task,
            NextStep::ExtractEvidence,
            "assertion left 13 right 12",
        );
        task.current_failure = Some(CurrentFailure {
            kind: "test_failure".to_string(),
            summary: "assertion left 13 right 12".to_string(),
            locations: vec!["src/lib.rs:42".to_string()],
        });
        assert_eq!(select_next_step(&task, 2), NextStep::PlanContext);
    }

    #[test]
    fn debug_failure_reads_context_when_next_actions_exist() {
        let mut task = task_with_intent(TaskIntent::DebugFailure);
        task.project = Some(ProjectCard {
            language: "Rust".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
        task.verification = Some(VerificationPlan {
            command: vec!["cargo".to_string(), "test".to_string()],
            expected_baseline_exit: Some(101),
            expected_final_exit: 0,
        });
        push_route(&mut task, NextStep::RunBaseline, "fail exit 101");
        push_route(
            &mut task,
            NextStep::ExtractEvidence,
            "assertion left 13 right 12",
        );
        push_route(&mut task, NextStep::PlanContext, "read src/lib.rs");
        task.next_actions.push(NextAction {
            command: "haycut read-window src/lib.rs --line 42".to_string(),
            reason: "inspect".to_string(),
            expected_answer: "why it fails".to_string(),
            estimated_tokens: 500,
            hypothesis: None,
        });
        assert_eq!(select_next_step(&task, 2), NextStep::ReadContext);
    }

    #[test]
    fn debug_failure_plans_patch_after_context() {
        let mut task = task_with_intent(TaskIntent::DebugFailure);
        task.project = Some(ProjectCard {
            language: "Rust".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
        task.verification = Some(VerificationPlan {
            command: vec!["cargo".to_string(), "test".to_string()],
            expected_baseline_exit: Some(101),
            expected_final_exit: 0,
        });
        push_route(&mut task, NextStep::RunBaseline, "fail exit 101");
        push_route(
            &mut task,
            NextStep::ExtractEvidence,
            "assertion left 13 right 12",
        );
        push_route(&mut task, NextStep::PlanContext, "read src/lib.rs");
        push_route(&mut task, NextStep::ReadContext, "window src/lib.rs:42");
        task.current_failure = Some(CurrentFailure {
            kind: "test_failure".to_string(),
            summary: "assertion left 13 right 12".to_string(),
            locations: vec!["src/lib.rs:42".to_string()],
        });
        assert_eq!(select_next_step(&task, 2), NextStep::PlanPatch);
    }

    #[test]
    fn debug_failure_applies_patch_then_verifies() {
        let mut task = task_with_intent(TaskIntent::DebugFailure);
        task.project = Some(ProjectCard {
            language: "Rust".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
        task.verification = Some(VerificationPlan {
            command: vec!["cargo".to_string(), "test".to_string()],
            expected_baseline_exit: Some(101),
            expected_final_exit: 0,
        });
        push_route(&mut task, NextStep::RunBaseline, "fail exit 101");
        push_route(
            &mut task,
            NextStep::ExtractEvidence,
            "assertion left 13 right 12",
        );
        push_route(&mut task, NextStep::PlanContext, "read src/lib.rs");
        push_route(&mut task, NextStep::ReadContext, "window src/lib.rs:42");
        push_route(&mut task, NextStep::PlanPatch, "change expected value");
        task.patch_text = Some("change expected value".to_string());
        assert_eq!(select_next_step(&task, 2), NextStep::ApplyPatch);

        push_route(&mut task, NextStep::ApplyPatch, "planned, not applied");
        task.patch_applied = true;
        assert_eq!(select_next_step(&task, 2), NextStep::RunFinalVerification);
    }

    #[test]
    fn debug_failure_reports_when_final_verification_passes() {
        let mut task = task_with_intent(TaskIntent::DebugFailure);
        task.project = Some(ProjectCard {
            language: "Rust".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
        task.verification = Some(VerificationPlan {
            command: vec!["cargo".to_string(), "test".to_string()],
            expected_baseline_exit: Some(101),
            expected_final_exit: 0,
        });
        push_route(&mut task, NextStep::RunBaseline, "fail exit 101");
        push_route(
            &mut task,
            NextStep::ExtractEvidence,
            "assertion left 13 right 12",
        );
        push_route(&mut task, NextStep::PlanContext, "read src/lib.rs");
        push_route(&mut task, NextStep::ReadContext, "window src/lib.rs:42");
        push_route(&mut task, NextStep::PlanPatch, "change expected value");
        push_route(&mut task, NextStep::ApplyPatch, "planned, not applied");
        push_route(&mut task, NextStep::RunFinalVerification, "pass exit 0");
        task.patch_text = Some("change expected value".to_string());
        task.patch_applied = true;
        assert_eq!(select_next_step(&task, 2), NextStep::Report);
    }

    #[test]
    fn answer_question_skips_patch_and_verifies() {
        let mut task = task_with_intent(TaskIntent::AnswerQuestion);
        task.project = Some(ProjectCard {
            language: "Rust".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
        assert_eq!(select_next_step(&task, 2), NextStep::DirectAnswer);
    }

    #[test]
    fn budget_exhausted_stops() {
        let mut task = empty_task();
        task.budget.packet_tokens_used = task.budget.hard_tokens;
        assert_eq!(
            select_next_step(&task, 2),
            NextStep::Stop(StopReason::BudgetExhausted)
        );
    }

    #[test]
    fn loop_detected_on_same_failure_signature() {
        let mut task = task_with_intent(TaskIntent::DebugFailure);
        task.project = Some(ProjectCard {
            language: "Rust".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
        task.verification = Some(VerificationPlan {
            command: vec!["cargo".to_string(), "test".to_string()],
            expected_baseline_exit: Some(101),
            expected_final_exit: 0,
        });
        task.current_failure = Some(CurrentFailure {
            kind: "test_failure".to_string(),
            summary: "assertion left 13 right 12".to_string(),
            locations: vec!["src/lib.rs:42".to_string()],
        });
        task.last_failure_signature = task.current_failure.as_ref().map(failure_signature);
        task.patch_text = Some("change expected value".to_string());
        task.patch_applied = true;
        push_route(&mut task, NextStep::RunFinalVerification, "fail exit 101");

        assert_eq!(
            select_next_step(&task, 2),
            NextStep::Stop(StopReason::LoopDetected)
        );
    }

    #[test]
    fn retry_allowed_when_failure_signature_changes() {
        let mut task = task_with_intent(TaskIntent::DebugFailure);
        task.project = Some(ProjectCard {
            language: "Rust".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
        task.verification = Some(VerificationPlan {
            command: vec!["cargo".to_string(), "test".to_string()],
            expected_baseline_exit: Some(101),
            expected_final_exit: 0,
        });
        task.current_failure = Some(CurrentFailure {
            kind: "test_failure".to_string(),
            summary: "new failure".to_string(),
            locations: vec!["src/lib.rs:42".to_string()],
        });
        task.last_failure_signature = Some("old-signature".to_string());
        task.patch_text = Some("change expected value".to_string());
        task.patch_applied = true;
        task.retry_count = 0;
        push_route(&mut task, NextStep::RunFinalVerification, "fail exit 101");

        assert_eq!(select_next_step(&task, 2), NextStep::RetryFix);
    }

    #[test]
    fn max_retries_stop() {
        let mut task = task_with_intent(TaskIntent::DebugFailure);
        task.project = Some(ProjectCard {
            language: "Rust".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
        task.verification = Some(VerificationPlan {
            command: vec!["cargo".to_string(), "test".to_string()],
            expected_baseline_exit: Some(101),
            expected_final_exit: 0,
        });
        task.current_failure = Some(CurrentFailure {
            kind: "test_failure".to_string(),
            summary: "new failure".to_string(),
            locations: vec!["src/lib.rs:42".to_string()],
        });
        task.last_failure_signature = Some("old-signature".to_string());
        task.patch_text = Some("change expected value".to_string());
        task.patch_applied = true;
        task.retry_count = 2;
        push_route(&mut task, NextStep::RunFinalVerification, "fail exit 101");

        assert_eq!(
            select_next_step(&task, 2),
            NextStep::Stop(StopReason::BudgetExhausted)
        );
    }

    #[test]
    fn failure_signature_is_stable() {
        let failure = CurrentFailure {
            kind: "test_failure".to_string(),
            summary: "assertion left 13 right 12".to_string(),
            locations: vec!["src/lib.rs:42".to_string()],
        };
        let sig1 = failure_signature(&failure);
        let sig2 = failure_signature(&failure);
        assert_eq!(sig1, sig2);
        assert!(!sig1.is_empty());
    }
}
