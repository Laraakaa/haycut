use serde::{Deserialize, Serialize};

use crate::commands::{
    agent::{
        primitive::{self, ContextCategory, PrimitiveId, PrimitiveVersion},
        workflow::Workflow,
        workflow_spec::{WorkflowGuard, WorkflowSpec},
    },
    eval::{CheckResult, Verdict},
    task::{RouteEntry, TaskState},
};
use crate::context::comparison::ContextCompilationComparison;
use crate::store::{StoredAgentTrace, StoredRequestManifest};

fn default_billed() -> bool {
    true
}

/// Where a run came from. Both kinds render through the same `RunDetail`
/// shape so the frontend has one code path for eval analysis today and live
/// agent progress later.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Eval,
    Task,
}

/// One row in the run list sidebar.
#[derive(Clone, Debug, Serialize)]
pub struct RunSummaryView {
    pub id: String,
    pub kind: RunKind,
    pub title: String,
    /// `pass`/`warn`/`fail` for finished evals, `open`/`closed` for tasks.
    pub status: String,
    pub started_at: Option<String>,
    pub total_model_tokens: Option<i64>,
    pub total_context_tokens: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BudgetView {
    pub packet_tokens_used: usize,
    pub soft_tokens: usize,
    pub hard_tokens: usize,
    pub max_tokens: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenSummaryView {
    pub packet_input_tokens: i64,
    pub model_input_tokens: i64,
    pub model_output_tokens: i64,
    pub total_model_tokens: i64,
    pub total_context_tokens: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunEntryView {
    pub id: String,
    pub command: String,
    pub exit_code: Option<i32>,
    pub raw_tokens: Option<i64>,
    pub packet_tokens: Option<i64>,
}

/// One LLM call: prompt/response/action/observation plus estimated vs
/// provider-reported token counts, for the call inspector.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepView {
    pub step_index: i64,
    pub model: String,
    pub purpose: String,
    #[serde(default = "default_billed")]
    pub billed: bool,
    pub prompt: String,
    pub response: String,
    pub action_json: String,
    pub observation: String,
    pub estimated_input_tokens: i64,
    pub estimated_output_tokens: i64,
    pub reported_input_tokens: Option<i64>,
    pub reported_output_tokens: Option<i64>,
    pub input_estimation_error: Option<i64>,
    pub input_estimation_ratio: Option<f64>,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelUsageView {
    pub model: String,
    pub purpose: String,
    #[serde(default = "default_billed")]
    pub billed: bool,
    pub calls: usize,
    pub estimated_input_tokens: i64,
    pub estimated_output_tokens: i64,
    pub reported_input_tokens: i64,
    pub reported_output_tokens: i64,
    pub input_estimation_error: i64,
    pub input_estimation_ratio: Option<f64>,
}

/// One off-site symbol surfaced by call-graph follow, judged relevant or not
/// by the weak model before the strong planner ever sees its body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AvailableContextView {
    pub id: String,
    pub symbol: String,
    pub path: String,
    pub relevant: Option<bool>,
}

/// One node of the compiled workflow DAG for a task.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowNodeSpecView {
    pub id: String,
    pub primitive_id: PrimitiveId,
    pub primitive_version: PrimitiveVersion,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub guard: Option<WorkflowGuard>,
}

/// The compiled `WorkflowSpec` for a task: entrypoints plus the DAG of nodes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowSpecView {
    pub schema_version: u8,
    pub compiler_version: String,
    pub entrypoints: Vec<String>,
    pub nodes: Vec<WorkflowNodeSpecView>,
}

impl From<&WorkflowSpec> for WorkflowSpecView {
    fn from(spec: &WorkflowSpec) -> Self {
        WorkflowSpecView {
            schema_version: spec.schema_version,
            compiler_version: spec.compiler_version.clone(),
            entrypoints: spec.entrypoints.clone(),
            nodes: spec
                .nodes
                .iter()
                .map(|node| WorkflowNodeSpecView {
                    id: node.id.clone(),
                    primitive_id: node.primitive_id.clone(),
                    primitive_version: node.primitive_version,
                    dependencies: node.dependencies.clone(),
                    guard: node.guard,
                })
                .collect(),
        }
    }
}

/// Static, run-independent listing of one registered primitive, replacing
/// the frontend's previously hardcoded executor/phase lookup table.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrimitiveSpecView {
    pub id: PrimitiveId,
    pub version: PrimitiveVersion,
    pub phase: String,
    pub executor: crate::commands::agent::workflow::ExecutorKind,
    pub required_context: Vec<ContextCategory>,
    pub optional_context: Vec<ContextCategory>,
}

fn primitive_registry_view() -> Vec<PrimitiveSpecView> {
    primitive::registry()
        .iter()
        .map(|entry| PrimitiveSpecView {
            id: entry.spec.id.clone(),
            version: entry.spec.version,
            phase: entry.spec.phase.as_str().to_string(),
            executor: entry.spec.executor,
            required_context: entry.spec.required_context.clone(),
            optional_context: entry.spec.optional_context.clone(),
        })
        .collect()
}

/// One typed context segment inside a request manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestManifestSegmentView {
    pub segment_id: String,
    pub position: i64,
    pub role: String,
    pub category: String,
    pub representation: String,
    pub producer_id: String,
    pub producer_version: i64,
    pub content_digest: String,
    pub dependency_digests_json: String,
    pub byte_size: i64,
    pub estimated_tokens: i64,
    pub cache_policy: String,
}

/// One prepared/completed LLM request manifest, with the shadow-mode
/// legacy-vs-compiled comparison verdict when available.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestManifestView {
    pub id: String,
    pub step_index: i64,
    pub node_id: Option<String>,
    pub primitive_id: String,
    pub primitive_version: i64,
    pub phase: String,
    pub model: String,
    pub purpose: String,
    pub status: String,
    pub estimated_input_tokens: i64,
    pub estimated_output_tokens: i64,
    pub reported_input_tokens: Option<i64>,
    pub reported_output_tokens: Option<i64>,
    pub billed: bool,
    pub error_summary: Option<String>,
    pub latency_ms: Option<i64>,
    pub prepared_at: String,
    pub completed_at: Option<String>,
    pub segments: Vec<RequestManifestSegmentView>,
    pub comparison: Option<ContextCompilationComparison>,
}

impl From<StoredRequestManifest> for RequestManifestView {
    fn from(manifest: StoredRequestManifest) -> Self {
        let comparison = manifest
            .comparison_json
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok());
        RequestManifestView {
            id: manifest.id,
            step_index: manifest.step_index,
            node_id: manifest.node_id,
            primitive_id: manifest.primitive_id,
            primitive_version: manifest.primitive_version,
            phase: manifest.phase,
            model: manifest.model,
            purpose: manifest.purpose,
            status: manifest.status,
            estimated_input_tokens: manifest.estimated_input_tokens,
            estimated_output_tokens: manifest.estimated_output_tokens,
            reported_input_tokens: manifest.reported_input_tokens,
            reported_output_tokens: manifest.reported_output_tokens,
            billed: manifest.billed,
            error_summary: manifest.error_summary,
            latency_ms: manifest.latency_ms,
            prepared_at: manifest.prepared_at,
            completed_at: manifest.completed_at,
            segments: manifest
                .segments
                .into_iter()
                .map(|segment| RequestManifestSegmentView {
                    segment_id: segment.segment_id,
                    position: segment.position,
                    role: segment.role,
                    category: segment.category,
                    representation: segment.representation,
                    producer_id: segment.producer_id,
                    producer_version: segment.producer_version,
                    content_digest: segment.content_digest,
                    dependency_digests_json: segment.dependency_digests_json,
                    byte_size: segment.byte_size,
                    estimated_tokens: segment.estimated_tokens,
                    cache_policy: segment.cache_policy,
                })
                .collect(),
            comparison,
        }
    }
}

/// Normalized view of one agent run, whether it came from an eval report.json
/// or a live (or completed) task in the SQLite store.
#[derive(Clone, Debug, Serialize)]
pub struct RunDetail {
    pub id: String,
    pub kind: RunKind,
    pub title: String,
    pub status: String,
    pub goal: String,
    pub verify: String,
    pub max_steps: Option<usize>,
    pub route: Vec<RouteEntry>,
    pub workflow: Workflow,
    pub terminal_reason: Option<crate::commands::agent::StopReason>,
    pub budget: BudgetView,
    pub token_summary: TokenSummaryView,
    pub steps: Vec<StepView>,
    pub model_usage: Vec<ModelUsageView>,
    pub runs: Vec<RunEntryView>,
    pub checks: Vec<CheckResult>,
    pub overall: Option<Verdict>,
    pub patch_text: Option<String>,
    /// Structured per-check outcomes from the task's most recent
    /// `RunFinalVerification` step (empty until verification has run).
    pub verification_results: Vec<crate::commands::task::VerificationCheckResult>,
    /// Structured file operations proposed by the current patch plan
    /// (Replace/Create/Delete/Rename), alongside the human-readable
    /// `patch_text` summary above.
    pub patch_edits: Vec<crate::commands::task::PatchEdit>,
    /// Question the interactive session is blocked on, awaiting a user
    /// reply (see `agent::engine`). `None` for eval runs and tasks that
    /// aren't currently blocked on a question.
    pub pending_interaction: Option<crate::commands::agent::engine::PendingInteraction>,
    /// Proposed mutation awaiting explicit user approval or rejection in an
    /// interactive session. `None` for eval runs and unblocked tasks.
    pub pending_approval: Option<crate::commands::agent::engine::ApprovalRequest>,
    /// Durable transcript of user/agent messages exchanged in an
    /// interactive session (questions, replies, rejections, steering
    /// instructions). Empty for eval runs.
    pub messages: Vec<crate::commands::agent::engine::TaskMessage>,
    pub available_context: Vec<AvailableContextView>,
    pub workflow_spec: Option<WorkflowSpecView>,
    pub manifests: Vec<RequestManifestView>,
    pub primitives: Vec<PrimitiveSpecView>,
}

/// One typed context segment inside an eval report's request manifest.
/// Mirrors `RequestSegmentReport` in `src/commands/eval.rs`.
#[derive(Clone, Debug, Deserialize)]
pub struct EvalRequestSegmentFile {
    pub id: String,
    pub position: i64,
    pub role: String,
    pub category: String,
    pub representation: String,
    pub producer_id: String,
    pub producer_version: i64,
    pub content_digest: String,
    pub byte_size: i64,
    pub estimated_tokens: i64,
    pub cache_policy: String,
}

/// One request manifest inside an eval report. Mirrors `RequestReport` in
/// `src/commands/eval.rs`, which has no `prepared_at`/`completed_at`
/// timestamps (only `StoredRequestManifest` does).
#[derive(Clone, Debug, Deserialize)]
pub struct EvalRequestFile {
    pub id: String,
    pub step_index: i64,
    pub node_id: Option<String>,
    pub primitive_id: String,
    pub primitive_version: i64,
    pub phase: String,
    pub model: String,
    pub purpose: String,
    pub status: String,
    pub estimated_input_tokens: i64,
    pub estimated_output_tokens: i64,
    pub reported_input_tokens: Option<i64>,
    pub reported_output_tokens: Option<i64>,
    pub billed: bool,
    pub error_summary: Option<String>,
    pub latency_ms: Option<i64>,
    pub segments: Vec<EvalRequestSegmentFile>,
    pub comparison: Option<ContextCompilationComparison>,
}

impl EvalRequestFile {
    fn into_view(self) -> RequestManifestView {
        RequestManifestView {
            id: self.id,
            step_index: self.step_index,
            node_id: self.node_id,
            primitive_id: self.primitive_id,
            primitive_version: self.primitive_version,
            phase: self.phase,
            model: self.model,
            purpose: self.purpose,
            status: self.status,
            estimated_input_tokens: self.estimated_input_tokens,
            estimated_output_tokens: self.estimated_output_tokens,
            reported_input_tokens: self.reported_input_tokens,
            reported_output_tokens: self.reported_output_tokens,
            billed: self.billed,
            error_summary: self.error_summary,
            latency_ms: self.latency_ms,
            prepared_at: String::new(),
            completed_at: None,
            segments: self
                .segments
                .into_iter()
                .map(|segment| RequestManifestSegmentView {
                    segment_id: segment.id,
                    position: segment.position,
                    role: segment.role,
                    category: segment.category,
                    representation: segment.representation,
                    producer_id: segment.producer_id,
                    producer_version: segment.producer_version,
                    content_digest: segment.content_digest,
                    dependency_digests_json: "{}".to_string(),
                    byte_size: segment.byte_size,
                    estimated_tokens: segment.estimated_tokens,
                    cache_policy: segment.cache_policy,
                })
                .collect(),
            comparison: self.comparison,
        }
    }
}

/// Deserialize mirror of the private `EvalReport` written by `haycut eval
/// run` (see `src/commands/eval.rs`). Kept separate from `EvalReport` itself
/// so the eval writer isn't coupled to the viewer's needs.
#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub struct EvalReportFile {
    #[serde(default)]
    pub schema_version: u8,
    pub case: String,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: String,
    #[serde(default)]
    pub agent_exit_code: Option<i32>,
    pub goal: String,
    pub verify: String,
    pub max_steps: usize,
    #[serde(default)]
    pub route: Vec<RouteEntry>,
    #[serde(default)]
    pub workflow: Workflow,
    #[serde(default)]
    pub workflow_spec: Option<WorkflowSpecView>,
    #[serde(default)]
    pub requests: Vec<EvalRequestFile>,
    #[serde(default)]
    pub terminal_reason: Option<crate::commands::agent::StopReason>,
    pub budget: BudgetView,
    pub token_summary: TokenSummaryView,
    #[serde(default)]
    pub runs: Vec<RunEntryView>,
    #[serde(default)]
    pub steps: Vec<StepView>,
    #[serde(default)]
    pub model_usage: Vec<ModelUsageView>,
    #[serde(default)]
    pub patch_text: Option<String>,
    #[serde(default)]
    pub verification_results: Vec<crate::commands::task::VerificationCheckResult>,
    #[serde(default)]
    pub checks: Vec<CheckResult>,
    pub overall: Verdict,
}

impl EvalReportFile {
    pub fn into_detail(self, id: String) -> RunDetail {
        RunDetail {
            id,
            kind: RunKind::Eval,
            title: self.case,
            status: verdict_label(&self.overall).to_string(),
            goal: self.goal,
            verify: self.verify,
            max_steps: Some(self.max_steps),
            route: self.route,
            workflow: self.workflow,
            terminal_reason: self.terminal_reason,
            budget: self.budget,
            token_summary: self.token_summary,
            steps: self.steps,
            model_usage: self.model_usage,
            runs: self.runs,
            checks: self.checks,
            overall: Some(self.overall),
            patch_text: self.patch_text,
            verification_results: self.verification_results,
            patch_edits: Vec::new(),
            pending_interaction: None,
            pending_approval: None,
            messages: Vec::new(),
            available_context: Vec::new(),
            workflow_spec: self.workflow_spec,
            manifests: self
                .requests
                .into_iter()
                .map(EvalRequestFile::into_view)
                .collect(),
            primitives: primitive_registry_view(),
        }
    }

    pub fn to_summary(&self, id: String) -> RunSummaryView {
        RunSummaryView {
            id,
            kind: RunKind::Eval,
            title: self.case.clone(),
            status: verdict_label(&self.overall).to_string(),
            started_at: Some(self.started_at.clone()),
            total_model_tokens: Some(self.token_summary.total_model_tokens),
            total_context_tokens: Some(self.token_summary.total_context_tokens),
        }
    }
}

fn verdict_label(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Pass => "pass",
        Verdict::Warn => "warn",
        Verdict::Fail => "fail",
    }
}

/// Builds a `RunDetail` for a live or completed task from its stored state
/// and recorded agent traces. Shares the exact shape `EvalReportFile`
/// produces, so the frontend needs no per-kind branching.
pub fn task_to_detail(
    id: String,
    status: &str,
    task: &TaskState,
    traces: &[StoredAgentTrace],
    manifests: Vec<StoredRequestManifest>,
) -> RunDetail {
    let steps: Vec<StepView> = traces.iter().map(trace_to_step).collect();
    let model_usage = aggregate_model_usage(&steps);

    let packet_input_tokens: i64 = task.runs.iter().map(|run| run.packet_tokens as i64).sum();
    let model_input_tokens: i64 = steps
        .iter()
        .map(|step| {
            step.reported_input_tokens
                .unwrap_or(step.estimated_input_tokens)
        })
        .sum();
    let model_output_tokens: i64 = steps
        .iter()
        .map(|step| {
            step.reported_output_tokens
                .unwrap_or(step.estimated_output_tokens)
        })
        .sum();

    let runs = task
        .runs
        .iter()
        .map(|run| RunEntryView {
            id: run.id.clone(),
            command: run.command.clone(),
            exit_code: Some(run.exit_code),
            raw_tokens: Some(run.raw_tokens as i64),
            packet_tokens: Some(run.packet_tokens as i64),
        })
        .collect();

    let available_context = task
        .available_context
        .iter()
        .map(|context| AvailableContextView {
            id: context.id.clone(),
            symbol: context.symbol.clone(),
            path: context.path.clone(),
            relevant: context.relevant,
        })
        .collect();

    RunDetail {
        id,
        kind: RunKind::Task,
        title: task.title.clone(),
        status: status.to_string(),
        goal: task.goal.clone(),
        verify: task
            .verification
            .as_ref()
            .map(|plan| {
                plan.checks
                    .iter()
                    .map(|check| check.command.display())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        max_steps: None,
        route: task.route.clone(),
        workflow: task.workflow.clone(),
        terminal_reason: task.terminal_reason,
        budget: BudgetView {
            packet_tokens_used: task.budget.packet_tokens_used,
            soft_tokens: task.budget.soft_tokens,
            hard_tokens: task.budget.hard_tokens,
            max_tokens: None,
        },
        token_summary: TokenSummaryView {
            packet_input_tokens,
            model_input_tokens,
            model_output_tokens,
            total_model_tokens: model_input_tokens + model_output_tokens,
            total_context_tokens: packet_input_tokens + model_input_tokens,
        },
        steps,
        model_usage,
        runs,
        checks: Vec::new(),
        overall: None,
        patch_text: task.patch_text.clone(),
        verification_results: task.verification_results.clone(),
        patch_edits: task.patch_edits.clone().unwrap_or_default(),
        pending_interaction: task.pending_interaction.clone(),
        pending_approval: task.pending_approval.clone(),
        messages: task.messages.clone(),
        available_context,
        workflow_spec: task.workflow_spec.as_ref().map(WorkflowSpecView::from),
        manifests: manifests
            .into_iter()
            .map(RequestManifestView::from)
            .collect(),
        primitives: primitive_registry_view(),
    }
}

pub fn task_to_summary(id: String, status: &str, task: &TaskState) -> RunSummaryView {
    RunSummaryView {
        id,
        kind: RunKind::Task,
        title: task.title.clone(),
        status: status.to_string(),
        started_at: None,
        total_model_tokens: None,
        total_context_tokens: None,
    }
}

fn trace_to_step(trace: &StoredAgentTrace) -> StepView {
    let input_estimation_error = trace
        .reported_input_tokens
        .map(|reported| reported - trace.estimated_input_tokens);
    let input_estimation_ratio = trace.reported_input_tokens.and_then(|reported| {
        if trace.estimated_input_tokens == 0 {
            None
        } else {
            Some(reported as f64 / trace.estimated_input_tokens as f64)
        }
    });

    StepView {
        step_index: trace.step_index,
        model: trace.model.clone(),
        purpose: trace.purpose.clone(),
        billed: trace.billed,
        prompt: trace.prompt.clone(),
        response: trace.response.clone(),
        action_json: trace.action_json.clone(),
        observation: trace.observation.clone(),
        estimated_input_tokens: trace.estimated_input_tokens,
        estimated_output_tokens: trace.estimated_output_tokens,
        reported_input_tokens: trace.reported_input_tokens,
        reported_output_tokens: trace.reported_output_tokens,
        input_estimation_error,
        input_estimation_ratio,
        created_at: Some(trace.created_at.clone()),
    }
}

fn aggregate_model_usage(steps: &[StepView]) -> Vec<ModelUsageView> {
    let mut usage: Vec<ModelUsageView> = Vec::new();

    for step in steps {
        if let Some(entry) = usage
            .iter_mut()
            .find(|entry| entry.model == step.model && entry.purpose == step.purpose)
        {
            entry.calls += 1;
            entry.estimated_input_tokens += step.estimated_input_tokens;
            entry.estimated_output_tokens += step.estimated_output_tokens;
            entry.reported_input_tokens += step
                .reported_input_tokens
                .unwrap_or(step.estimated_input_tokens);
            entry.reported_output_tokens += step
                .reported_output_tokens
                .unwrap_or(step.estimated_output_tokens);
        } else {
            usage.push(ModelUsageView {
                model: step.model.clone(),
                purpose: step.purpose.clone(),
                billed: step.billed,
                calls: 1,
                estimated_input_tokens: step.estimated_input_tokens,
                estimated_output_tokens: step.estimated_output_tokens,
                reported_input_tokens: step
                    .reported_input_tokens
                    .unwrap_or(step.estimated_input_tokens),
                reported_output_tokens: step
                    .reported_output_tokens
                    .unwrap_or(step.estimated_output_tokens),
                input_estimation_error: 0,
                input_estimation_ratio: None,
            });
        }
    }

    for entry in &mut usage {
        entry.input_estimation_error = entry.reported_input_tokens - entry.estimated_input_tokens;
        entry.input_estimation_ratio = if entry.estimated_input_tokens == 0 {
            None
        } else {
            Some(entry.reported_input_tokens as f64 / entry.estimated_input_tokens as f64)
        };
    }

    usage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_report_file_surfaces_requests_and_workflow_spec() {
        let json = r#"{
            "case": "example_rs",
            "started_at": "2026-07-16T00:00:00Z",
            "goal": "fix it",
            "verify": "cargo test",
            "max_steps": 10,
            "budget": {"packet_tokens_used": 0, "soft_tokens": 100, "hard_tokens": 200, "max_tokens": null},
            "token_summary": {"packet_input_tokens": 0, "model_input_tokens": 0, "model_output_tokens": 0, "total_model_tokens": 0, "total_context_tokens": 0},
            "overall": "pass",
            "workflow_spec": {
                "schema_version": 1,
                "compiler_version": "v1",
                "entrypoints": ["n1"],
                "nodes": [{
                    "id": "n1",
                    "primitive_id": "classify_intent",
                    "primitive_version": 1
                }]
            },
            "requests": [
                {
                    "id": "req1",
                    "step_index": 0,
                    "node_id": null,
                    "primitive_id": "plan_patch",
                    "primitive_version": 1,
                    "phase": "patch",
                    "model": "strong",
                    "purpose": "patch_generation",
                    "status": "completed",
                    "estimated_input_tokens": 10,
                    "estimated_output_tokens": 5,
                    "reported_input_tokens": null,
                    "reported_output_tokens": null,
                    "billed": true,
                    "error_summary": null,
                    "latency_ms": null,
                    "segments": [],
                    "comparison": null
                }
            ]
        }"#;

        let file: EvalReportFile = serde_json::from_str(json).expect("deserializes");
        let detail = file.into_detail("run-1".to_string());

        assert!(detail.workflow_spec.is_some());
        assert_eq!(detail.manifests.len(), 1);
        assert_eq!(detail.manifests[0].id, "req1");
        assert!(
            detail.workflow_spec.unwrap().nodes[0]
                .dependencies
                .is_empty()
        );
    }
}
