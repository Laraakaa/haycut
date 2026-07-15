use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::commands::task::{CurrentFailure, TaskIntent, TaskState};

/// Stable, monotonic node identifier ("n1", "n2", ...).
pub type NodeId = String;

/// The concrete operation a graph node performs. Maps 1:1 onto the
/// `execute_*` functions in `agent.rs` — no new behaviour, only a new
/// structural home for what was `NextStep`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeOp {
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
    /// Cheap weak-model selection of which off-site symbols the failure
    /// depends on, resolved deterministically into context.
    SelectContext,
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

/// What kind of executor should run a node.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    /// No model call: pure code/rules.
    Deterministic,
    /// Cheap model: intent classification, ranking, summarisation.
    WeakModel,
    /// Capable model: planning, patch generation, direct answers.
    StrongModel,
    /// External process: test/build commands.
    Command,
}

impl NodeOp {
    #[allow(dead_code)]
    pub const ALL: [Self; 15] = [
        Self::ClassifyIntent,
        Self::DetectProject,
        Self::ResolveVerification,
        Self::RunBaseline,
        Self::ExtractEvidence,
        Self::SelectContext,
        Self::PlanContext,
        Self::ReadContext,
        Self::PlanPatch,
        Self::ApplyPatch,
        Self::RunFinalVerification,
        Self::RetryFix,
        Self::AskUser,
        Self::DirectAnswer,
        Self::Report,
    ];

    #[allow(dead_code)]
    pub fn all() -> &'static [Self] {
        &Self::ALL
    }

    /// Executor kind that should run this node.
    pub fn executor(&self) -> ExecutorKind {
        match self {
            NodeOp::ClassifyIntent => ExecutorKind::WeakModel,
            NodeOp::DetectProject => ExecutorKind::Deterministic,
            NodeOp::ResolveVerification => ExecutorKind::Deterministic,
            NodeOp::RunBaseline => ExecutorKind::Command,
            NodeOp::ExtractEvidence => ExecutorKind::Deterministic,
            NodeOp::SelectContext => ExecutorKind::WeakModel,
            NodeOp::PlanContext => ExecutorKind::StrongModel,
            NodeOp::ReadContext => ExecutorKind::Deterministic,
            NodeOp::PlanPatch => ExecutorKind::StrongModel,
            NodeOp::ApplyPatch => ExecutorKind::Deterministic,
            NodeOp::RunFinalVerification => ExecutorKind::Command,
            NodeOp::RetryFix => ExecutorKind::Deterministic,
            NodeOp::AskUser => ExecutorKind::Deterministic,
            NodeOp::DirectAnswer => ExecutorKind::StrongModel,
            // `execute_report` only formats the already-computed patch_text /
            // task-complete summary; it never calls a model.
            NodeOp::Report => ExecutorKind::Deterministic,
        }
    }

    /// Short snake_case name used in route logging and tests.
    pub fn name(&self) -> &'static str {
        match self {
            NodeOp::ClassifyIntent => "classify_intent",
            NodeOp::DetectProject => "detect_project",
            NodeOp::ResolveVerification => "resolve_verification",
            NodeOp::RunBaseline => "run_baseline",
            NodeOp::ExtractEvidence => "extract_evidence",
            NodeOp::SelectContext => "select_context",
            NodeOp::PlanContext => "plan_context",
            NodeOp::ReadContext => "read_context",
            NodeOp::PlanPatch => "plan_patch",
            NodeOp::ApplyPatch => "apply_patch",
            NodeOp::RunFinalVerification => "run_final_verification",
            NodeOp::RetryFix => "retry_fix",
            NodeOp::AskUser => "ask_user",
            NodeOp::DirectAnswer => "direct_answer",
            NodeOp::Report => "report",
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

/// Lifecycle of a graph node.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

/// A single node in the task's workflow graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub op: NodeOp,
    #[serde(default)]
    pub depends_on: Vec<NodeId>,
    pub status: NodeStatus,
    #[serde(default)]
    pub produced_by: Option<NodeId>,
    #[serde(default)]
    pub outcome: Option<String>,
}

/// The task's self-writing DAG: the graph is the source of truth for what
/// runs next. Nodes are appended dynamically as `next_ready_node` decides
/// what the task needs, wired to the node that produced the decision.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Workflow {
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub seq: u32,
}

impl Workflow {
    /// A fresh task's workflow: a single seed node awaiting classification.
    pub fn new() -> Self {
        let mut workflow = Workflow::default();
        workflow.push(NodeOp::ClassifyIntent, Vec::new(), None);
        workflow
    }

    fn push(&mut self, op: NodeOp, depends_on: Vec<NodeId>, produced_by: Option<NodeId>) -> NodeId {
        self.seq += 1;
        let id = format!("n{}", self.seq);
        self.nodes.push(Node {
            id: id.clone(),
            op,
            depends_on,
            status: NodeStatus::Pending,
            produced_by,
            outcome: None,
        });
        id
    }

    fn node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|node| node.id == id)
    }

    pub fn mark_running(&mut self, id: &str) {
        if let Some(node) = self.node_mut(id) {
            node.status = NodeStatus::Running;
        }
    }

    /// Mark a node done, upserting it if it is missing (some executors
    /// reload the task mid-step, which can drop an in-memory, not-yet-saved
    /// node — this keeps the graph consistent regardless).
    pub fn complete(&mut self, id: NodeId, op: NodeOp, outcome: String) {
        match self.node_mut(&id) {
            Some(node) => {
                node.status = NodeStatus::Done;
                node.outcome = Some(outcome);
            }
            None => {
                self.nodes.push(Node {
                    id,
                    op,
                    depends_on: Vec::new(),
                    status: NodeStatus::Done,
                    produced_by: None,
                    outcome: Some(outcome),
                });
            }
        }
    }

    pub fn mark_failed(&mut self, id: &str) {
        if let Some(node) = self.node_mut(id) {
            node.status = NodeStatus::Failed;
        }
    }
}

/// The outcome of asking the graph what runs next.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    Ready(NodeId, NodeOp),
    Stop(StopReason),
}

/// Pure, testable decision: given the current task state, decide the next
/// operation. No side effects, no model calls.
fn decide(task: &TaskState, max_retries: usize) -> Result<NodeOp, StopReason> {
    // Hard stop: budget exhausted.
    if task.budget.packet_tokens_used >= task.budget.hard_tokens {
        return Err(StopReason::BudgetExhausted);
    }

    let Some(intent) = task.intent else {
        return Ok(NodeOp::ClassifyIntent);
    };

    let policy = IntentPolicy::for_intent(intent);

    // Final-verification loop: if the last final verification failed, decide
    // whether to retry, loop-detect, give up, or ask the user.
    if let Some(last_final) = task
        .route
        .iter()
        .rev()
        .find(|entry| entry.step == NodeOp::RunFinalVerification.name())
    {
        let failed = !outcome_passed(&last_final.outcome);
        if failed {
            return resolve_retry(task, max_retries);
        }
    }

    if task.project.is_none() {
        return Ok(NodeOp::DetectProject);
    }

    if policy.requires_verification && task.verification.is_none() {
        return Ok(NodeOp::ResolveVerification);
    }

    if policy.requires_verification && !has_step(task, NodeOp::RunBaseline.name()) {
        return Ok(NodeOp::RunBaseline);
    }

    if policy.expect_baseline_failure
        && task.current_failure.is_none()
        && !has_step(task, NodeOp::ExtractEvidence.name())
    {
        return Ok(NodeOp::ExtractEvidence);
    }

    if policy.patch_expected
        && task.current_failure.is_some()
        && !has_step(task, NodeOp::SelectContext.name())
    {
        return Ok(NodeOp::SelectContext);
    }

    if needs_more_context(task, policy) {
        if task.next_actions.is_empty() {
            return Ok(NodeOp::PlanContext);
        }
        return Ok(NodeOp::ReadContext);
    }

    if policy.patch_expected {
        if task.patch_text.is_none() {
            return Ok(NodeOp::PlanPatch);
        }
        if !task.patch_applied {
            if task.patch_previewed {
                return Err(StopReason::Blocked);
            }
            return Ok(NodeOp::ApplyPatch);
        }
        if policy.require_final_verification && !has_step(task, NodeOp::RunFinalVerification.name())
        {
            return Ok(NodeOp::RunFinalVerification);
        }
    }

    if !policy.patch_expected && !has_step(task, NodeOp::DirectAnswer.name()) {
        return Ok(NodeOp::DirectAnswer);
    }

    if !has_step(task, NodeOp::Report.name()) {
        return Ok(NodeOp::Report);
    }

    let verified = policy.require_final_verification && has_passing_final_verification(task)
        || !policy.patch_expected;
    Err(if verified {
        StopReason::Verified
    } else {
        StopReason::Blocked
    })
}

/// Ask the graph which node is ready to run next, appending it (chained to
/// the last-completed node) if it doesn't already exist as a pending node.
/// This is the self-writing step: each call may extend the graph based on
/// what the task has learned so far.
pub fn next_ready_node(workflow: &mut Workflow, task: &TaskState, max_retries: usize) -> Decision {
    match decide(task, max_retries) {
        Err(reason) => Decision::Stop(reason),
        Ok(op) => {
            if let Some(node) = workflow
                .nodes
                .iter()
                .find(|node| node.status == NodeStatus::Pending && node.op == op)
            {
                Decision::Ready(node.id.clone(), op)
            } else {
                let parent = workflow
                    .nodes
                    .iter()
                    .rev()
                    .find(|node| node.status == NodeStatus::Done)
                    .map(|node| node.id.clone());
                let depends_on = parent.iter().cloned().collect();
                let id = workflow.push(op, depends_on, parent);
                Decision::Ready(id, op)
            }
        }
    }
}

/// Per-intent policy flags controlling the graph's route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IntentPolicy {
    pub(crate) needs_repo: bool,
    pub(crate) requires_verification: bool,
    pub(crate) expect_baseline_failure: bool,
    pub(crate) patch_expected: bool,
    pub(crate) require_final_verification: bool,
}

impl IntentPolicy {
    pub(crate) fn for_intent(intent: TaskIntent) -> Self {
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

/// Decide what to do after a failed final verification.
fn resolve_retry(task: &TaskState, max_retries: usize) -> Result<NodeOp, StopReason> {
    if task.retry_count >= max_retries {
        return Err(StopReason::Failed);
    }

    let current_signature = task.current_failure.as_ref().map(failure_signature);
    let previous_signature = task.last_failure_signature.clone();

    if current_signature.is_some() && current_signature == previous_signature {
        return Err(StopReason::LoopDetected);
    }

    if task.patch_text.is_some() {
        return Ok(NodeOp::RetryFix);
    }

    if task.budget.packet_tokens_used >= task.budget.soft_tokens {
        return Ok(NodeOp::AskUser);
    }

    Err(StopReason::Blocked)
}

/// Whether the agent still needs more context before it can plan a patch or
/// answer. Uses existing observations, hypotheses, and next_actions.
fn needs_more_context(task: &TaskState, policy: IntentPolicy) -> bool {
    if !policy.needs_repo {
        return false;
    }

    if !policy.patch_expected {
        return false;
    }

    if !task.next_actions.is_empty() {
        return true;
    }

    if task.patch_text.is_none()
        && !has_step(task, NodeOp::PlanContext.name())
        && !has_step(task, NodeOp::ReadContext.name())
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
        .find(|entry| entry.step == NodeOp::RunFinalVerification.name())
        .map(|entry| outcome_passed(&entry.outcome))
        .unwrap_or(false)
}

/// Parse a command outcome string for pass/fail.
fn outcome_passed(outcome: &str) -> bool {
    let outcome = outcome.trim();
    outcome.starts_with("pass")
        || outcome == "0"
        || outcome.contains("exit 0")
        || outcome.contains("exited 0")
        || outcome.contains("(pass)")
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
            patch_edits: None,
            patch_applied: false,
            patch_previewed: false,
            apply_requested: false,
            terminal_reason: None,
            retry_count: 0,
            last_failure_signature: None,
            available_context: Vec::new(),
            workflow_spec: None,
            workflow: Workflow::new(),
        }
    }

    fn task_with_intent(intent: TaskIntent) -> TaskState {
        let mut task = empty_task();
        task.intent = Some(intent);
        task
    }

    fn push_route(task: &mut TaskState, step: NodeOp, outcome: &str) {
        task.route.push(RouteEntry {
            step: step.name().to_string(),
            executor: step.executor(),
            primitive_id: None,
            primitive_version: None,
            outcome: outcome.to_string(),
        });
    }

    fn decision(task: &TaskState, max_retries: usize) -> Decision {
        let mut workflow = task.workflow.clone();
        next_ready_node(&mut workflow, task, max_retries)
    }

    #[test]
    fn empty_task_starts_with_classify() {
        let task = empty_task();
        assert_eq!(
            decision(&task, 2),
            Decision::Ready("n1".to_string(), NodeOp::ClassifyIntent)
        );
    }

    #[test]
    fn classified_task_detects_project() {
        let task = task_with_intent(TaskIntent::DebugFailure);
        assert!(matches!(
            decision(&task, 2),
            Decision::Ready(_, NodeOp::DetectProject)
        ));
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
        assert!(matches!(
            decision(&task, 2),
            Decision::Ready(_, NodeOp::RunBaseline)
        ));
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
        push_route(&mut task, NodeOp::RunBaseline, "fail exit 101");
        assert!(matches!(
            decision(&task, 2),
            Decision::Ready(_, NodeOp::ExtractEvidence)
        ));
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
        push_route(&mut task, NodeOp::RunBaseline, "fail exit 101");
        push_route(
            &mut task,
            NodeOp::ExtractEvidence,
            "assertion left 13 right 12",
        );
        task.current_failure = Some(CurrentFailure {
            kind: "test_failure".to_string(),
            summary: "assertion left 13 right 12".to_string(),
            locations: vec!["src/lib.rs:42".to_string()],
            detail: None,
        });
        assert!(matches!(
            decision(&task, 2),
            Decision::Ready(_, NodeOp::SelectContext)
        ));
        push_route(&mut task, NodeOp::SelectContext, "no off-site symbols");
        assert!(matches!(
            decision(&task, 2),
            Decision::Ready(_, NodeOp::PlanContext)
        ));
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
        push_route(&mut task, NodeOp::RunBaseline, "fail exit 101");
        push_route(
            &mut task,
            NodeOp::ExtractEvidence,
            "assertion left 13 right 12",
        );
        push_route(&mut task, NodeOp::PlanContext, "read src/lib.rs");
        task.next_actions.push(NextAction {
            command: "haycut read-window src/lib.rs --line 42".to_string(),
            reason: "inspect".to_string(),
            expected_answer: "why it fails".to_string(),
            estimated_tokens: 500,
            hypothesis: None,
        });
        assert!(matches!(
            decision(&task, 2),
            Decision::Ready(_, NodeOp::ReadContext)
        ));
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
        push_route(&mut task, NodeOp::RunBaseline, "fail exit 101");
        push_route(
            &mut task,
            NodeOp::ExtractEvidence,
            "assertion left 13 right 12",
        );
        push_route(&mut task, NodeOp::SelectContext, "no off-site symbols");
        push_route(&mut task, NodeOp::PlanContext, "read src/lib.rs");
        push_route(&mut task, NodeOp::ReadContext, "window src/lib.rs:42");
        task.current_failure = Some(CurrentFailure {
            kind: "test_failure".to_string(),
            summary: "assertion left 13 right 12".to_string(),
            locations: vec!["src/lib.rs:42".to_string()],
            detail: None,
        });
        assert!(matches!(
            decision(&task, 2),
            Decision::Ready(_, NodeOp::PlanPatch)
        ));
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
        push_route(&mut task, NodeOp::RunBaseline, "fail exit 101");
        push_route(
            &mut task,
            NodeOp::ExtractEvidence,
            "assertion left 13 right 12",
        );
        push_route(&mut task, NodeOp::PlanContext, "read src/lib.rs");
        push_route(&mut task, NodeOp::ReadContext, "window src/lib.rs:42");
        push_route(&mut task, NodeOp::PlanPatch, "change expected value");
        task.patch_text = Some("change expected value".to_string());
        assert!(matches!(
            decision(&task, 2),
            Decision::Ready(_, NodeOp::ApplyPatch)
        ));

        push_route(&mut task, NodeOp::ApplyPatch, "planned, not applied");
        task.patch_applied = true;
        assert!(matches!(
            decision(&task, 2),
            Decision::Ready(_, NodeOp::RunFinalVerification)
        ));
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
        push_route(&mut task, NodeOp::RunBaseline, "fail exit 101");
        push_route(
            &mut task,
            NodeOp::ExtractEvidence,
            "assertion left 13 right 12",
        );
        push_route(&mut task, NodeOp::PlanContext, "read src/lib.rs");
        push_route(&mut task, NodeOp::ReadContext, "window src/lib.rs:42");
        push_route(&mut task, NodeOp::PlanPatch, "change expected value");
        push_route(&mut task, NodeOp::ApplyPatch, "applied 1 edit(s)");
        push_route(
            &mut task,
            NodeOp::RunFinalVerification,
            "final verification `cargo test` exited 0 (pass)",
        );
        task.patch_text = Some("change expected value".to_string());
        task.patch_applied = true;
        assert!(matches!(
            decision(&task, 2),
            Decision::Ready(_, NodeOp::Report)
        ));
    }

    #[test]
    fn answer_question_skips_patch_and_verifies() {
        let mut task = task_with_intent(TaskIntent::AnswerQuestion);
        task.project = Some(ProjectCard {
            language: "Rust".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
        assert!(matches!(
            decision(&task, 2),
            Decision::Ready(_, NodeOp::DirectAnswer)
        ));
    }

    #[test]
    fn budget_exhausted_stops() {
        let mut task = empty_task();
        task.budget.packet_tokens_used = task.budget.hard_tokens;
        assert_eq!(
            decision(&task, 2),
            Decision::Stop(StopReason::BudgetExhausted)
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
            detail: None,
        });
        task.last_failure_signature = task.current_failure.as_ref().map(failure_signature);
        task.patch_text = Some("change expected value".to_string());
        task.patch_applied = true;
        push_route(&mut task, NodeOp::RunFinalVerification, "fail exit 101");

        assert_eq!(decision(&task, 2), Decision::Stop(StopReason::LoopDetected));
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
            detail: None,
        });
        task.last_failure_signature = Some("old-signature".to_string());
        task.patch_text = Some("change expected value".to_string());
        task.patch_applied = true;
        task.retry_count = 0;
        push_route(&mut task, NodeOp::RunFinalVerification, "fail exit 101");

        assert!(matches!(
            decision(&task, 2),
            Decision::Ready(_, NodeOp::RetryFix)
        ));
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
            detail: None,
        });
        task.last_failure_signature = Some("old-signature".to_string());
        task.patch_text = Some("change expected value".to_string());
        task.patch_applied = true;
        task.retry_count = 2;
        push_route(&mut task, NodeOp::RunFinalVerification, "fail exit 101");

        assert_eq!(decision(&task, 2), Decision::Stop(StopReason::Failed));
    }

    #[test]
    fn failure_signature_is_stable() {
        let failure = CurrentFailure {
            kind: "test_failure".to_string(),
            summary: "assertion left 13 right 12".to_string(),
            locations: vec!["src/lib.rs:42".to_string()],
            detail: None,
        };
        let sig1 = failure_signature(&failure);
        let sig2 = failure_signature(&failure);
        assert_eq!(sig1, sig2);
        assert!(!sig1.is_empty());
    }

    #[test]
    fn graph_extends_with_dependency_chain() {
        let task = empty_task();
        let mut workflow = task.workflow.clone();
        let first = next_ready_node(&mut workflow, &task, 2);
        let Decision::Ready(seed_id, NodeOp::ClassifyIntent) = first else {
            panic!("expected seeded ClassifyIntent node");
        };
        assert_eq!(workflow.nodes.len(), 1);
        workflow.complete(seed_id.clone(), NodeOp::ClassifyIntent, "debug".to_string());

        let mut task2 = task_with_intent(TaskIntent::DebugFailure);
        task2.workflow = workflow;
        let second = next_ready_node(&mut task2.workflow.clone(), &task2, 2);
        let Decision::Ready(next_id, NodeOp::DetectProject) = second else {
            panic!("expected DetectProject node");
        };
        assert_ne!(next_id, seed_id);
    }
}
