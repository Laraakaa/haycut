use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    commands::task::{RouteEntry, TaskBudget, TaskState},
    store::{
        self, RUN_STORE_PATH, RunSummary, StoredAgentTrace, StoredRequestManifest,
        StoredRequestManifestSegment,
    },
};

// ---------------------------------------------------------------------
// Case definitions
// ---------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EvalCase {
    pub goal: String,
    pub verify: String,
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,
    pub checks: CheckConfig,
    /// Name the case was loaded under (not part of the TOML file).
    #[serde(skip)]
    pub name: String,
    /// Directory containing `repo/` (not part of the TOML file).
    #[serde(skip)]
    pub dir: PathBuf,
}

fn default_max_steps() -> usize {
    12
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CheckConfig {
    pub max_tokens: usize,
    #[serde(default)]
    pub expected_markers: Vec<String>,
    #[serde(default)]
    pub relevant_paths: Vec<String>,
    #[serde(default = "default_max_read_radius")]
    pub max_read_radius: usize,
}

fn default_max_read_radius() -> usize {
    60
}

pub fn load_case(cases_dir: &Path, name: &str) -> io::Result<EvalCase> {
    let dir = cases_dir.join(name);
    let case_path = dir.join("case.toml");
    let contents = fs::read_to_string(&case_path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to read {}: {error}", case_path.display()),
        )
    })?;

    let mut case: EvalCase = toml::from_str(&contents).map_err(io::Error::other)?;
    case.name = name.to_string();
    case.dir = dir;

    Ok(case)
}

pub fn list_cases(cases_dir: &Path) -> io::Result<Vec<String>> {
    if !cases_dir.exists() {
        return Ok(Vec::new());
    }

    let mut names = Vec::new();
    for entry in fs::read_dir(cases_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("case.toml").exists() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

// ---------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------

pub struct RunOutcome {
    pub workspace: PathBuf,
    pub agent_exit_code: Option<i32>,
}

pub fn run_agent_in_workspace(case: &EvalCase, repo_dir: &Path) -> io::Result<RunOutcome> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after Unix epoch")
        .as_nanos();
    let workspace = std::env::temp_dir().join(format!("haycut-eval-{}-{nanos}", case.name));

    copy_dir_recursive(repo_dir, &workspace)?;

    let exe = std::env::current_exe()?;
    // The agent is not told the verify command; it resolves verification via
    // project detection. The harness keeps `case.verify` only for its own
    // records/checks. Flags precede the goal so the goal positional does not
    // slurp them.
    let status = Command::new(exe)
        .args([
            "agent",
            "run",
            "--apply",
            "--max-steps",
            &case.max_steps.to_string(),
            &case.goal,
        ])
        .current_dir(&workspace)
        .status()?;

    Ok(RunOutcome {
        workspace,
        agent_exit_code: status.code(),
    })
}

pub fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Evidence collection
// ---------------------------------------------------------------------

pub struct EvalEvidence {
    pub task: TaskState,
    pub traces: Vec<StoredAgentTrace>,
    pub runs: Vec<RunSummary>,
    pub manifests: Vec<StoredRequestManifest>,
}

pub fn collect_evidence(workspace: &Path) -> io::Result<EvalEvidence> {
    let db_path = workspace.join(RUN_STORE_PATH);
    let stored_task = store::current_task(&db_path)?;
    let task: TaskState = serde_json::from_str(&stored_task.task_json)?;
    let traces = store::agent_traces_for_task(&db_path, &stored_task.id)?;
    let runs = store::recent_runs(&db_path, 100)?;
    let manifests = store::request_manifests_for_task(&db_path, &stored_task.id)?;

    Ok(EvalEvidence {
        task,
        traces,
        runs,
        manifests,
    })
}

pub fn copy_workspace_db(workspace: &Path, dest: &Path) -> io::Result<()> {
    let db_path = workspace.join(RUN_STORE_PATH);
    if db_path.exists() {
        fs::copy(&db_path, dest)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Heuristics
// ---------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Warn,
    Fail,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub verdict: Verdict,
    pub reasons: Vec<String>,
}

pub fn check_token_budget(checks: &CheckConfig, task: &TaskBudget) -> CheckResult {
    if task.packet_tokens_used <= checks.max_tokens {
        CheckResult {
            name: "token_budget".to_string(),
            verdict: Verdict::Pass,
            reasons: vec![format!(
                "{} tokens used <= {} budget",
                task.packet_tokens_used, checks.max_tokens
            )],
        }
    } else {
        CheckResult {
            name: "token_budget".to_string(),
            verdict: Verdict::Fail,
            reasons: vec![format!(
                "{} tokens used exceeds {} budget",
                task.packet_tokens_used, checks.max_tokens
            )],
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ActionShape {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    radius: Option<usize>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
}

pub fn check_wasteful_actions(checks: &CheckConfig, traces: &[StoredAgentTrace]) -> CheckResult {
    let mut reasons = Vec::new();
    let mut verdict = Verdict::Pass;
    let mut seen_actions: Vec<&str> = Vec::new();

    for trace in traces {
        let action: ActionShape = match serde_json::from_str(&trace.action_json) {
            Ok(action) => action,
            Err(_) => continue,
        };

        if seen_actions.contains(&trace.action_json.as_str()) {
            verdict = Verdict::Fail;
            reasons.push(format!(
                "step {} repeats a previous action verbatim",
                trace.step_index
            ));
        }
        seen_actions.push(&trace.action_json);

        if let Some(radius) = action.radius {
            if radius > checks.max_read_radius {
                if verdict != Verdict::Fail {
                    verdict = Verdict::Warn;
                }
                reasons.push(format!(
                    "step {} read window radius {radius} exceeds {}",
                    trace.step_index, checks.max_read_radius
                ));
            }
        } else if action.action.as_deref() == Some("read_window") {
            if verdict != Verdict::Fail {
                verdict = Verdict::Warn;
            }
            reasons.push(format!(
                "step {} performed an un-anchored whole-file read",
                trace.step_index
            ));
        }

        if !checks.relevant_paths.is_empty()
            && let Some(file) = &action.file
            && !checks.relevant_paths.iter().any(|path| path == file)
        {
            if verdict != Verdict::Fail {
                verdict = Verdict::Warn;
            }
            reasons.push(format!(
                "step {} read {file}, outside relevant paths",
                trace.step_index
            ));
        }

        let _ = (&action.command, &action.args);
    }

    CheckResult {
        name: "wasteful_actions".to_string(),
        verdict,
        reasons,
    }
}

pub fn check_outcome_markers(checks: &CheckConfig, task: &TaskState) -> CheckResult {
    let mut haystack = String::new();
    if let Some(patch_text) = &task.patch_text {
        haystack.push_str(patch_text);
        haystack.push('\n');
    }
    for observation in &task.observations {
        haystack.push_str(&observation.summary);
        haystack.push('\n');
    }
    for hypothesis in &task.hypotheses {
        haystack.push_str(&hypothesis.summary);
        haystack.push('\n');
    }
    let haystack_lower = haystack.to_lowercase();

    let missing: Vec<String> = checks
        .expected_markers
        .iter()
        .filter(|marker| !haystack_lower.contains(&marker.to_lowercase()))
        .cloned()
        .collect();

    if missing.is_empty() {
        CheckResult {
            name: "outcome_markers".to_string(),
            verdict: Verdict::Pass,
            reasons: vec!["all expected markers found".to_string()],
        }
    } else {
        CheckResult {
            name: "outcome_markers".to_string(),
            verdict: Verdict::Fail,
            reasons: vec![format!("missing markers: {}", missing.join(", "))],
        }
    }
}

pub fn evaluate(case: &EvalCase, evidence: &EvalEvidence) -> Vec<CheckResult> {
    vec![
        check_token_budget(&case.checks, &evidence.task.budget),
        check_wasteful_actions(&case.checks, &evidence.traces),
        check_outcome_markers(&case.checks, &evidence.task),
        check_request_manifests(evidence),
    ]
}

fn check_request_manifests(evidence: &EvalEvidence) -> CheckResult {
    let missing: Vec<_> = evidence
        .traces
        .iter()
        .filter(|trace| {
            trace.manifest_id.as_ref().is_none_or(|manifest_id| {
                !evidence.manifests.iter().any(|manifest| {
                    manifest.id == *manifest_id
                        && matches!(
                            manifest.status.as_str(),
                            "completed" | "provider_failed" | "recording_failed"
                        )
                })
            })
        })
        .map(|trace| format!("step {} {}", trace.step_index, trace.purpose))
        .collect();
    let incomplete: Vec<_> = evidence
        .manifests
        .iter()
        .filter(|manifest| manifest.status == "prepared")
        .map(|manifest| manifest.id.clone())
        .collect();

    if missing.is_empty() && incomplete.is_empty() {
        CheckResult {
            name: "request_manifests".to_string(),
            verdict: Verdict::Pass,
            reasons: vec![format!(
                "{} model attempts have explicit outcomes",
                evidence.manifests.len()
            )],
        }
    } else {
        let mut reasons = Vec::new();
        if !missing.is_empty() {
            reasons.push(format!("traces missing manifests: {}", missing.join(", ")));
        }
        if !incomplete.is_empty() {
            reasons.push(format!(
                "prepared manifests remain: {}",
                incomplete.join(", ")
            ));
        }
        CheckResult {
            name: "request_manifests".to_string(),
            verdict: Verdict::Fail,
            reasons,
        }
    }
}

pub fn overall_verdict(results: &[CheckResult]) -> Verdict {
    if results.iter().any(|result| result.verdict == Verdict::Fail) {
        Verdict::Fail
    } else if results.iter().any(|result| result.verdict == Verdict::Warn) {
        Verdict::Warn
    } else {
        Verdict::Pass
    }
}

// ---------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------

#[derive(Serialize)]
struct StepReport {
    step_index: i64,
    model: String,
    purpose: String,
    billed: bool,
    prompt: String,
    response: String,
    action_json: String,
    observation: String,
    estimated_input_tokens: i64,
    estimated_output_tokens: i64,
    reported_input_tokens: Option<i64>,
    reported_output_tokens: Option<i64>,
    /// `reported_input_tokens - estimated_input_tokens`; positive means the
    /// local estimator under-counted what the provider actually billed.
    input_estimation_error: Option<i64>,
    /// `reported_input_tokens / estimated_input_tokens`; `None` when there is
    /// no reported figure or the estimate is zero.
    input_estimation_ratio: Option<f64>,
}

#[derive(Serialize)]
struct ModelUsageReport {
    model: String,
    purpose: String,
    billed: bool,
    calls: usize,
    estimated_input_tokens: i64,
    estimated_output_tokens: i64,
    reported_input_tokens: i64,
    reported_output_tokens: i64,
    /// Aggregate `reported_input_tokens - estimated_input_tokens` across all
    /// calls of this (model, purpose).
    input_estimation_error: i64,
    /// Aggregate `reported_input_tokens / estimated_input_tokens`; `None`
    /// when the estimated total is zero.
    input_estimation_ratio: Option<f64>,
}

#[derive(Serialize)]
struct RunReport {
    id: String,
    command: String,
    exit_code: Option<i32>,
    raw_tokens: Option<i64>,
    packet_tokens: Option<i64>,
}

#[derive(Serialize)]
struct BudgetReport {
    packet_tokens_used: usize,
    soft_tokens: usize,
    hard_tokens: usize,
    max_tokens: usize,
}

/// Breaks `packet_tokens_used` apart into what it's actually made of, so a
/// single combined counter doesn't hide whether the cost is repo/tool
/// evidence or model calls.
#[derive(Serialize)]
struct TokenSummary {
    /// Tokens spent packetizing command/tool evidence (e.g. `cargo test`
    /// output), summed from `runs`.
    packet_input_tokens: i64,
    /// Model input tokens across all recorded calls: reported by the
    /// provider where available, falling back to the local estimate.
    model_input_tokens: i64,
    /// Model output tokens, same fallback rule as `model_input_tokens`.
    model_output_tokens: i64,
    /// `model_input_tokens + model_output_tokens`: total billable model
    /// tokens for this run.
    total_model_tokens: i64,
    /// `packet_input_tokens + model_input_tokens`. Note: when packet
    /// evidence text is later embedded verbatim into a model prompt (e.g.
    /// observation summaries in the patch-plan prompt), those tokens are
    /// counted once as evidence and again as model input, so this is an
    /// upper bound on distinct context tokens, not a deduplicated count.
    total_context_tokens: i64,
}

#[derive(Serialize)]
struct RequestSummary {
    request_count: usize,
    prepared_count: usize,
    completed_count: usize,
    failed_count: usize,
    segment_count: usize,
    estimated_input_tokens: i64,
    estimated_output_tokens: i64,
    reported_input_tokens: i64,
    reported_output_tokens: i64,
    reported_cached_input_tokens: i64,
    fresh_input_tokens: i64,
    cache_ratio: Option<f64>,
    latency_total_ms: i64,
    latency_p50_ms: Option<i64>,
    latency_p95_ms: Option<i64>,
}

#[derive(Serialize)]
struct RequestReport {
    id: String,
    step_index: i64,
    node_id: Option<String>,
    primitive_id: String,
    primitive_version: i64,
    phase: String,
    model: String,
    purpose: String,
    request_digest: String,
    status: String,
    estimated_input_tokens: i64,
    estimated_output_tokens: i64,
    reported_input_tokens: Option<i64>,
    reported_output_tokens: Option<i64>,
    reported_cached_input_tokens: Option<i64>,
    provider_request_id: Option<String>,
    latency_ms: Option<i64>,
    billed: bool,
    error_summary: Option<String>,
    comparison: Option<serde_json::Value>,
    segments: Vec<RequestSegmentReport>,
}

#[derive(Serialize)]
struct RequestSegmentReport {
    id: String,
    position: i64,
    role: String,
    category: String,
    representation: String,
    producer_id: String,
    producer_version: i64,
    content_digest: String,
    byte_size: i64,
    estimated_tokens: i64,
    cache_policy: String,
}

#[derive(Serialize)]
pub struct EvalReport {
    schema_version: u8,
    case: String,
    started_at: String,
    finished_at: String,
    agent_exit_code: Option<i32>,
    goal: String,
    verify: String,
    max_steps: usize,
    route: Vec<RouteEntry>,
    workflow: crate::commands::agent::workflow::Workflow,
    terminal_reason: Option<crate::commands::agent::StopReason>,
    budget: BudgetReport,
    token_summary: TokenSummary,
    runs: Vec<RunReport>,
    steps: Vec<StepReport>,
    model_usage: Vec<ModelUsageReport>,
    request_summary: RequestSummary,
    requests: Vec<RequestReport>,
    patch_text: Option<String>,
    checks: Vec<CheckResult>,
    overall: Verdict,
}

pub fn build_report(
    case: &EvalCase,
    outcome: &RunOutcome,
    evidence: &EalEvidenceAlias,
    checks: Vec<CheckResult>,
    started_at: chrono::DateTime<Utc>,
) -> EvalReport {
    let overall = overall_verdict(&checks);

    EvalReport {
        schema_version: 2,
        case: case.name.clone(),
        started_at: started_at.to_rfc3339(),
        finished_at: Utc::now().to_rfc3339(),
        agent_exit_code: outcome.agent_exit_code,
        goal: case.goal.clone(),
        verify: case.verify.clone(),
        max_steps: case.max_steps,
        route: evidence.task.route.clone(),
        workflow: evidence.task.workflow.clone(),
        terminal_reason: evidence.task.terminal_reason,
        budget: BudgetReport {
            packet_tokens_used: evidence.task.budget.packet_tokens_used,
            soft_tokens: evidence.task.budget.soft_tokens,
            hard_tokens: evidence.task.budget.hard_tokens,
            max_tokens: case.checks.max_tokens,
        },
        token_summary: token_summary(&evidence.runs, &evidence.traces),
        runs: evidence
            .runs
            .iter()
            .map(|run| RunReport {
                id: run.id.clone(),
                command: run.command.clone(),
                exit_code: run.exit_code,
                raw_tokens: run.raw_tokens,
                packet_tokens: run.packet_tokens,
            })
            .collect(),
        steps: evidence
            .traces
            .iter()
            .map(|trace| StepReport {
                step_index: trace.step_index,
                model: trace.model.clone(),
                purpose: trace.purpose.clone(),
                billed: trace.billed,
                prompt: excerpt(&trace.prompt, 2000),
                response: excerpt(&trace.response, 2000),
                action_json: trace.action_json.clone(),
                observation: excerpt(&trace.observation, 500),
                estimated_input_tokens: trace.estimated_input_tokens,
                estimated_output_tokens: trace.estimated_output_tokens,
                reported_input_tokens: trace.reported_input_tokens,
                reported_output_tokens: trace.reported_output_tokens,
                input_estimation_error: trace
                    .reported_input_tokens
                    .map(|reported| reported - trace.estimated_input_tokens),
                input_estimation_ratio: estimation_ratio(
                    trace.estimated_input_tokens,
                    trace.reported_input_tokens,
                ),
            })
            .collect(),
        model_usage: model_usage_report(&evidence.traces),
        request_summary: request_summary(&evidence.manifests),
        requests: evidence.manifests.iter().map(request_report).collect(),
        patch_text: evidence.task.patch_text.clone(),
        checks,
        overall,
    }
}

fn request_summary(manifests: &[StoredRequestManifest]) -> RequestSummary {
    let reported_input_tokens: i64 = manifests
        .iter()
        .filter_map(|manifest| manifest.reported_input_tokens)
        .sum();
    let reported_cached_input_tokens: i64 = manifests
        .iter()
        .filter_map(|manifest| manifest.reported_cached_input_tokens)
        .sum();
    let mut latencies: Vec<_> = manifests
        .iter()
        .filter_map(|manifest| manifest.latency_ms)
        .collect();
    latencies.sort_unstable();

    RequestSummary {
        request_count: manifests.len(),
        prepared_count: manifests
            .iter()
            .filter(|manifest| manifest.status == "prepared")
            .count(),
        completed_count: manifests
            .iter()
            .filter(|manifest| manifest.status == "completed")
            .count(),
        failed_count: manifests
            .iter()
            .filter(|manifest| {
                matches!(
                    manifest.status.as_str(),
                    "provider_failed" | "recording_failed"
                )
            })
            .count(),
        segment_count: manifests
            .iter()
            .map(|manifest| manifest.segments.len())
            .sum(),
        estimated_input_tokens: manifests
            .iter()
            .map(|manifest| manifest.estimated_input_tokens)
            .sum(),
        estimated_output_tokens: manifests
            .iter()
            .map(|manifest| manifest.estimated_output_tokens)
            .sum(),
        reported_input_tokens,
        reported_output_tokens: manifests
            .iter()
            .filter_map(|manifest| manifest.reported_output_tokens)
            .sum(),
        reported_cached_input_tokens,
        fresh_input_tokens: reported_input_tokens.saturating_sub(reported_cached_input_tokens),
        cache_ratio: (reported_input_tokens > 0)
            .then_some(reported_cached_input_tokens as f64 / reported_input_tokens as f64),
        latency_total_ms: latencies.iter().sum(),
        latency_p50_ms: percentile(&latencies, 50),
        latency_p95_ms: percentile(&latencies, 95),
    }
}

fn percentile(sorted: &[i64], percentile: usize) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let index = ((sorted.len() - 1) * percentile).div_ceil(100);
    sorted.get(index).copied()
}

fn request_report(manifest: &StoredRequestManifest) -> RequestReport {
    RequestReport {
        id: manifest.id.clone(),
        step_index: manifest.step_index,
        node_id: manifest.node_id.clone(),
        primitive_id: manifest.primitive_id.clone(),
        primitive_version: manifest.primitive_version,
        phase: manifest.phase.clone(),
        model: manifest.model.clone(),
        purpose: manifest.purpose.clone(),
        request_digest: manifest.request_digest.clone(),
        status: manifest.status.clone(),
        estimated_input_tokens: manifest.estimated_input_tokens,
        estimated_output_tokens: manifest.estimated_output_tokens,
        reported_input_tokens: manifest.reported_input_tokens,
        reported_output_tokens: manifest.reported_output_tokens,
        reported_cached_input_tokens: manifest.reported_cached_input_tokens,
        provider_request_id: manifest.provider_request_id.clone(),
        latency_ms: manifest.latency_ms,
        billed: manifest.billed,
        error_summary: manifest.error_summary.clone(),
        comparison: manifest
            .comparison_json
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok()),
        segments: manifest
            .segments
            .iter()
            .map(request_segment_report)
            .collect(),
    }
}

fn request_segment_report(segment: &StoredRequestManifestSegment) -> RequestSegmentReport {
    RequestSegmentReport {
        id: segment.segment_id.clone(),
        position: segment.position,
        role: segment.role.clone(),
        category: segment.category.clone(),
        representation: segment.representation.clone(),
        producer_id: segment.producer_id.clone(),
        producer_version: segment.producer_version,
        content_digest: segment.content_digest.clone(),
        byte_size: segment.byte_size,
        estimated_tokens: segment.estimated_tokens,
        cache_policy: segment.cache_policy.clone(),
    }
}

/// Total tokens spent packetizing command/tool evidence, summed from `runs`.
fn packet_input_tokens(runs: &[RunSummary]) -> i64 {
    runs.iter().filter_map(|run| run.packet_tokens).sum()
}

/// `reported / estimated`, or `None` when there's no reported figure or the
/// estimate is zero (avoids a division that would otherwise be meaningless).
fn estimation_ratio(estimated: i64, reported: Option<i64>) -> Option<f64> {
    reported
        .filter(|_| estimated > 0)
        .map(|reported| reported as f64 / estimated as f64)
}

fn token_summary(runs: &[RunSummary], traces: &[StoredAgentTrace]) -> TokenSummary {
    let packet_input_tokens = packet_input_tokens(runs);
    let model_input_tokens: i64 = traces
        .iter()
        .map(|trace| {
            trace
                .reported_input_tokens
                .unwrap_or(trace.estimated_input_tokens)
        })
        .sum();
    let model_output_tokens: i64 = traces
        .iter()
        .map(|trace| {
            trace
                .reported_output_tokens
                .unwrap_or(trace.estimated_output_tokens)
        })
        .sum();

    TokenSummary {
        packet_input_tokens,
        model_input_tokens,
        model_output_tokens,
        total_model_tokens: model_input_tokens + model_output_tokens,
        total_context_tokens: packet_input_tokens + model_input_tokens,
    }
}

fn model_usage_report(traces: &[StoredAgentTrace]) -> Vec<ModelUsageReport> {
    let mut usage: Vec<ModelUsageReport> = Vec::new();
    for trace in traces {
        let entry = usage
            .iter_mut()
            .find(|entry| entry.model == trace.model && entry.purpose == trace.purpose);
        let entry = match entry {
            Some(entry) => entry,
            None => {
                usage.push(ModelUsageReport {
                    model: trace.model.clone(),
                    purpose: trace.purpose.clone(),
                    billed: trace.billed,
                    calls: 0,
                    estimated_input_tokens: 0,
                    estimated_output_tokens: 0,
                    reported_input_tokens: 0,
                    reported_output_tokens: 0,
                    input_estimation_error: 0,
                    input_estimation_ratio: None,
                });
                usage.last_mut().expect("just pushed")
            }
        };
        entry.calls += 1;
        entry.estimated_input_tokens += trace.estimated_input_tokens;
        entry.estimated_output_tokens += trace.estimated_output_tokens;
        entry.reported_input_tokens += trace.reported_input_tokens.unwrap_or(0);
        entry.reported_output_tokens += trace.reported_output_tokens.unwrap_or(0);
    }
    for entry in &mut usage {
        entry.input_estimation_error = entry.reported_input_tokens - entry.estimated_input_tokens;
        entry.input_estimation_ratio = estimation_ratio(
            entry.estimated_input_tokens,
            Some(entry.reported_input_tokens),
        );
    }
    usage
}

type EalEvidenceAlias = EvalEvidence;

fn excerpt(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_string()
    } else {
        format!("{}...", &text[..max_len])
    }
}

pub fn write_report(
    results_dir: &Path,
    case_name: &str,
    report: &EvalReport,
) -> io::Result<PathBuf> {
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ");
    let out_dir = results_dir.join(format!("{ts}-{case_name}"));
    fs::create_dir_all(&out_dir)?;

    let report_json = serde_json::to_string_pretty(report)?;
    fs::write(out_dir.join("report.json"), report_json)?;
    fs::write(out_dir.join("summary.txt"), render_summary(report))?;

    Ok(out_dir)
}

pub fn render_summary(report: &EvalReport) -> String {
    let mut summary = String::new();
    summary.push_str(&format!("case: {}\n", report.case));
    summary.push_str(&format!("overall: {:?}\n", report.overall));
    summary.push_str(&format!(
        "tokens: {} / {} (budget {})\n",
        report.budget.packet_tokens_used, report.budget.hard_tokens, report.budget.max_tokens
    ));
    summary.push_str(&format!(
        "token summary: packet_input={} model_input={} model_output={} total_model={} total_context={}\n",
        report.token_summary.packet_input_tokens,
        report.token_summary.model_input_tokens,
        report.token_summary.model_output_tokens,
        report.token_summary.total_model_tokens,
        report.token_summary.total_context_tokens,
    ));
    summary.push_str(&format!("steps: {}\n", report.steps.len()));
    summary.push_str("model usage:\n");
    for usage in &report.model_usage {
        summary.push_str(&format!(
            "  - {} [{}] calls={} estimated={}in/{}out reported={}in/{}out estimation_error={}in ({})\n",
            usage.model,
            usage.purpose,
            usage.calls,
            usage.estimated_input_tokens,
            usage.estimated_output_tokens,
            usage.reported_input_tokens,
            usage.reported_output_tokens,
            usage.input_estimation_error,
            usage
                .input_estimation_ratio
                .map(|ratio| format!("{ratio:.2}x"))
                .unwrap_or_else(|| "n/a".to_string()),
        ));
    }
    summary.push_str("llm calls:\n");
    for step in &report.steps {
        summary.push_str(&format!(
            "  [{}] {} ({}) reported={}in/{}out\n",
            step.step_index,
            step.model,
            step.purpose,
            step.reported_input_tokens.unwrap_or(0),
            step.reported_output_tokens.unwrap_or(0)
        ));
        summary.push_str(&format!("      prompt:   {}\n", excerpt(&step.prompt, 200)));
        summary.push_str(&format!(
            "      response: {}\n",
            excerpt(&step.response, 200)
        ));
    }
    summary.push_str("checks:\n");
    for check in &report.checks {
        summary.push_str(&format!("  - {} => {:?}\n", check.name, check.verdict));
        for reason in &check.reasons {
            summary.push_str(&format!("      {reason}\n"));
        }
    }
    summary
}

// ---------------------------------------------------------------------
// CLI entry points
// ---------------------------------------------------------------------

pub fn run_list(cases_dir: &Path) -> i32 {
    match list_cases(cases_dir) {
        Ok(names) => {
            if names.is_empty() {
                println!("no eval cases found in {}", cases_dir.display());
            } else {
                for name in names {
                    println!("{name}");
                }
            }
            0
        }
        Err(error) => {
            eprintln!("failed to list cases: {error}");
            2
        }
    }
}

pub fn run_case(cases_dir: &Path, results_dir: &Path, name: &str) -> i32 {
    let started_at = Utc::now();

    let case = match load_case(cases_dir, name) {
        Ok(case) => case,
        Err(error) => {
            eprintln!("failed to load case {name}: {error}");
            return 2;
        }
    };

    let repo_dir = case.dir.join("repo");
    let outcome = match run_agent_in_workspace(&case, &repo_dir) {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("failed to run agent: {error}");
            return 2;
        }
    };

    let evidence = match collect_evidence(&outcome.workspace) {
        Ok(evidence) => evidence,
        Err(error) => {
            eprintln!("failed to collect evidence: {error}");
            let _ = fs::remove_dir_all(&outcome.workspace);
            return 2;
        }
    };

    let checks = evaluate(&case, &evidence);
    let report = build_report(&case, &outcome, &evidence, checks, started_at);

    let out_dir = match write_report(results_dir, &case.name, &report) {
        Ok(out_dir) => out_dir,
        Err(error) => {
            eprintln!("failed to write report: {error}");
            let _ = fs::remove_dir_all(&outcome.workspace);
            return 2;
        }
    };

    if let Err(error) = copy_workspace_db(&outcome.workspace, &out_dir.join("workspace.sqlite3")) {
        eprintln!("warning: failed to copy workspace db: {error}");
    }

    if let Err(error) = copy_dir_recursive(&outcome.workspace, &out_dir.join("workspace")) {
        eprintln!("warning: failed to preserve workspace directory: {error}");
    }

    let _ = fs::remove_dir_all(&outcome.workspace);

    print!("{}", render_summary(&report));
    println!("report written to {}", out_dir.display());

    match report.overall {
        Verdict::Fail => 1,
        Verdict::Pass | Verdict::Warn => 0,
    }
}

#[allow(dead_code)]
fn read_all(mut reader: impl Read) -> io::Result<String> {
    let mut buffer = String::new();
    reader.read_to_string(&mut buffer)?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::task::{Observation, ObservationTokens, TaskBudget};

    fn base_task() -> TaskState {
        TaskState {
            schema_version: 1,
            id: "task-1".to_string(),
            title: "test".to_string(),
            goal: "goal".to_string(),
            acceptance: Vec::new(),
            constraints: Vec::new(),
            budget: TaskBudget {
                soft_tokens: 1000,
                hard_tokens: 2000,
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
            workflow: crate::commands::agent::workflow::Workflow::new(),
        }
    }

    fn checks() -> CheckConfig {
        CheckConfig {
            max_tokens: 100,
            expected_markers: vec!["sum".to_string(), "4".to_string()],
            relevant_paths: vec!["src/lib.rs".to_string()],
            max_read_radius: 60,
        }
    }

    #[test]
    fn token_budget_passes_under_ceiling() {
        let mut task = base_task();
        task.budget.packet_tokens_used = 50;
        let result = check_token_budget(&checks(), &task.budget);
        assert_eq!(result.verdict, Verdict::Pass);
    }

    #[test]
    fn token_budget_fails_over_ceiling() {
        let mut task = base_task();
        task.budget.packet_tokens_used = 500;
        let result = check_token_budget(&checks(), &task.budget);
        assert_eq!(result.verdict, Verdict::Fail);
    }

    fn trace(step: i64, action_json: &str) -> StoredAgentTrace {
        StoredAgentTrace {
            id: format!("trace-{step}"),
            task_id: "task-1".to_string(),
            step_index: step,
            model: "gpt-4o-mini".to_string(),
            purpose: "agent_planner".to_string(),
            prompt: String::new(),
            response: String::new(),
            action_json: action_json.to_string(),
            observation: String::new(),
            estimated_input_tokens: 0,
            estimated_output_tokens: 0,
            reported_input_tokens: None,
            reported_output_tokens: None,
            billed: true,
            manifest_id: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn wasteful_actions_flags_duplicate_action() {
        let traces = vec![
            trace(
                0,
                r#"{"action":"read_window","file":"src/lib.rs","radius":10}"#,
            ),
            trace(
                1,
                r#"{"action":"read_window","file":"src/lib.rs","radius":10}"#,
            ),
        ];
        let result = check_wasteful_actions(&checks(), &traces);
        assert_eq!(result.verdict, Verdict::Fail);
    }

    #[test]
    fn wasteful_actions_warns_on_large_radius() {
        let traces = vec![trace(
            0,
            r#"{"action":"read_window","file":"src/lib.rs","radius":200}"#,
        )];
        let result = check_wasteful_actions(&checks(), &traces);
        assert_eq!(result.verdict, Verdict::Warn);
    }

    #[test]
    fn wasteful_actions_warns_on_off_path_read() {
        let traces = vec![trace(
            0,
            r#"{"action":"read_window","file":"src/other.rs","radius":10}"#,
        )];
        let result = check_wasteful_actions(&checks(), &traces);
        assert_eq!(result.verdict, Verdict::Warn);
    }

    #[test]
    fn outcome_markers_pass_when_present_in_patch() {
        let mut task = base_task();
        task.patch_text = Some("fix sum to return 4".to_string());
        let result = check_outcome_markers(&checks(), &task);
        assert_eq!(result.verdict, Verdict::Pass);
    }

    #[test]
    fn outcome_markers_fail_when_missing() {
        let task = base_task();
        let result = check_outcome_markers(&checks(), &task);
        assert_eq!(result.verdict, Verdict::Fail);
    }

    #[test]
    fn outcome_markers_pass_via_observation_summary() {
        let mut task = base_task();
        task.observations.push(Observation {
            id: "obs-0".to_string(),
            source: "test".to_string(),
            kind: "test_failure".to_string(),
            summary: "sum returned 4 instead of 5".to_string(),
            locations: Vec::new(),
            tokens: ObservationTokens { raw: 0, packet: 0 },
        });
        let result = check_outcome_markers(&checks(), &task);
        assert_eq!(result.verdict, Verdict::Pass);
    }

    #[test]
    fn copy_dir_recursive_copies_nested_files() {
        let src = std::env::temp_dir().join(format!(
            "haycut-eval-test-src-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dst = std::env::temp_dir().join(format!(
            "haycut-eval-test-dst-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(src.join("nested/file.txt"), "hello").unwrap();

        copy_dir_recursive(&src, &dst).unwrap();

        assert_eq!(
            fs::read_to_string(dst.join("nested/file.txt")).unwrap(),
            "hello"
        );

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }

    #[test]
    fn load_case_reads_toml() {
        let cases_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/cases");
        let case = load_case(&cases_dir, "sum_wrong_assertion_rs").unwrap();
        assert_eq!(case.verify, "cargo test");
        assert_eq!(case.checks.max_tokens, 20000);
    }
}
