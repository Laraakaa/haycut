use serde::{Deserialize, Serialize};

use crate::commands::{
    agent::workflow::Workflow,
    eval::{CheckResult, Verdict},
    task::{RouteEntry, TaskState},
};
use crate::store::StoredAgentTrace;

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
    pub budget: BudgetView,
    pub token_summary: TokenSummaryView,
    pub steps: Vec<StepView>,
    pub model_usage: Vec<ModelUsageView>,
    pub runs: Vec<RunEntryView>,
    pub checks: Vec<CheckResult>,
    pub overall: Option<Verdict>,
    pub patch_text: Option<String>,
    pub available_context: Vec<AvailableContextView>,
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
            budget: self.budget,
            token_summary: self.token_summary,
            steps: self.steps,
            model_usage: self.model_usage,
            runs: self.runs,
            checks: self.checks,
            overall: Some(self.overall),
            patch_text: self.patch_text,
            available_context: Vec::new(),
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
) -> RunDetail {
    let steps: Vec<StepView> = traces.iter().map(trace_to_step).collect();
    let model_usage = aggregate_model_usage(&steps);

    let packet_input_tokens: i64 = task
        .runs
        .iter()
        .map(|run| run.packet_tokens as i64)
        .sum();
    let model_input_tokens: i64 = steps
        .iter()
        .map(|step| step.reported_input_tokens.unwrap_or(step.estimated_input_tokens))
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
            .map(|plan| plan.command.join(" "))
            .unwrap_or_default(),
        max_steps: None,
        route: task.route.clone(),
        workflow: task.workflow.clone(),
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
        available_context,
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
