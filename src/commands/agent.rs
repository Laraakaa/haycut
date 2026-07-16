use std::{
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use chrono::Utc;
use minijinja::{Environment, context};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    cli::{AgentCommand, TaskTarget},
    code_graph,
    commands::{read_symbol, read_window, search, task, trace},
    config::Config,
    context::{
        invocation,
        request::{
            self, AssembledRequest, CachePolicy, ContextRepresentation, ContextRole,
            ContextSegment, RequestAssembly, RequestCorrelation,
        },
    },
    model::{ModelPurpose, OpenAiProvider, ToolDefinition},
    store::{self, NewAgentTrace, RUN_STORE_PATH},
    util::{estimate_tokens, format_count},
};

pub mod command_policy;
pub mod engine;
pub mod patch;
pub mod primitive;
pub mod session;
pub mod workflow;
pub mod workflow_spec;
use workflow::{Decision, NodeOp};
pub use workflow::{ExecutorKind, StopReason};

pub const DEFAULT_MAX_STEPS: usize = 8;
const MAX_RETRIES: usize = 2;
const SEARCH_LIMIT: usize = 20;
const MAX_OUTPUT_TOKENS: usize = 512;
/// Patch generation answers with a short list of structured edits rather than
/// prose, so it needs far fewer output tokens than other strong-model calls.
const PATCH_MAX_OUTPUT_TOKENS: usize = 256;

/// Static policy given to the planner once per step. The per-tool "when to
/// use" guidance lives in each tool's schema description (cached alongside the
/// schema), so this stays a short, non-redundant behaviour policy.
const PLANNER_SYSTEM_PROMPT: &str = include_str!("../prompts/planner_system.txt");

/// The user prompt is assembled from a Jinja template so it is easy to see
/// what lands where. Compiled once and reused across steps.
const PLANNER_USER_TEMPLATE: &str = include_str!("../prompts/planner_user.jinja");

pub fn run(command: AgentCommand) -> i32 {
    match command {
        AgentCommand::Run {
            task,
            max_steps,
            apply,
            goal,
        } => run_loop(task, goal.join(" "), max_steps, apply),
        AgentCommand::Step { task } => run_step(task),
        AgentCommand::Trace { task } => run_trace(task),
        AgentCommand::Session { task, goal } => session::run(task, goal.join(" ")),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PlannerAction {
    pub action: ActionKind,
    #[serde(default)]
    pub args: ActionArgs,
    pub reason: String,
}

impl PlannerAction {
    fn action_name(&self) -> String {
        match self.action {
            ActionKind::Search => "search".to_string(),
            ActionKind::ReadSymbol => "read_symbol".to_string(),
            ActionKind::ReadWindow => "read_window".to_string(),
            ActionKind::Trace => "trace".to_string(),
            ActionKind::ProposePatchPlan => "plan".to_string(),
            ActionKind::Finish => "finish".to_string(),
            ActionKind::AskUser => "ask".to_string(),
            ActionKind::PullContext => "pull".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Search,
    ReadSymbol,
    ReadWindow,
    Trace,
    ProposePatchPlan,
    Finish,
    AskUser,
    PullContext,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ActionArgs {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<usize>,
    #[serde(default)]
    pub radius: Option<usize>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
}

/// Typed action threaded from planner output through to deterministic
/// execution, without ever being serialized into a shell-like command string
/// and re-parsed (the source of quoting bugs and impossible "unrecognised
/// queued action" states). A single `AgentAction` covers both the
/// context-gathering actions `ReadContext` executes and the terminal
/// decisions (`PlanPatch`/`AskUser`/`Finish`) that route the workflow
/// elsewhere; `PlanContext` sets whichever kind the planner chose, and only
/// context-gathering variants are ever consumed by `ReadContext`.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AgentAction {
    Search { query: String },
    ReadSymbol { target: String },
    ReadWindow { path: PathBuf, line: usize, radius: usize },
    RunCommand { program: String, args: Vec<String> },
    PullContext { id: String },
    PlanPatch,
    AskUser { question: String },
    Finish,
}

#[derive(Debug)]
struct StepResult {
    summary: String,
    terminal: bool,
}

struct AgentTraceInput<'a> {
    model: &'a str,
    purpose: &'a str,
    prompt: &'a str,
    response: &'a str,
    executor: ExecutorKind,
    input_tokens: usize,
    output_tokens: usize,
    observation: &'a str,
    billed: bool,
    manifest_id: Option<&'a str>,
}

fn run_loop(task_target: Option<TaskTarget>, goal: String, max_steps: usize, apply: bool) -> i32 {
    if task_target != Some(TaskTarget::Current) && goal.trim().is_empty() {
        eprintln!("Error: provide --task current or a goal");
        return 2;
    }

    if !goal.trim().is_empty() && task_target != Some(TaskTarget::Current) {
        // The agent resolves its own verification command via project
        // detection, so no verify command is seeded from the CLI.
        match task::start_current(goal, None) {
            Ok(task) => println!("Started task {}", task.id),
            Err(error) => {
                eprintln!("Error starting task: {error}");
                return 1;
            }
        }
    }

    let mut task = match task::load_current() {
        Ok(task) => task,
        Err(error) => {
            eprintln!("Error loading current task: {error}");
            return 1;
        }
    };
    task.apply_requested = apply;
    if apply {
        task.patch_previewed = false;
    }
    if !apply {
        println!("Patch mode: preview only. Use --apply to permit writes.");
    }

    for step in 0..max_steps {
        let mut workflow = task.workflow.clone();
        let (node_id, next) = match workflow::next_ready_node(&mut workflow, &task, MAX_RETRIES) {
            Decision::Stop(reason) => {
                print_stop(reason);
                task.terminal_reason = Some(reason);
                if let Err(error) = task::save_current(&task) {
                    eprintln!("Error saving task: {error}");
                    return 1;
                }
                return stop_exit_code(reason);
            }
            Decision::Ready(id, op) => (id, op),
        };
        workflow.mark_running(&node_id);
        task.workflow = workflow;
        // Some executors reload the task mid-step (`*task = task::load_current()`)
        // after running a command; persist the running node first so that
        // reload sees it instead of an older, node-less snapshot.
        if let Err(error) = task::save_current(&task) {
            eprintln!("Error saving task: {error}");
            return 1;
        }

        let step_index = task.route.len() + 1;
        match execute_step(&next, &mut task, step_index) {
            Ok(outcome) => {
                task.workflow
                    .complete(node_id, next, outcome.summary.clone());
                record_route(&mut task, &next, &outcome);
                println!(
                    "step {}: {} ({:?}) — {}",
                    step + 1,
                    next.name(),
                    next.executor(),
                    first_line(&outcome.summary)
                );
                if let Err(error) = task::save_current(&task) {
                    eprintln!("Error saving task: {error}");
                    return 1;
                }
                if outcome.terminal {
                    let mut workflow = task.workflow.clone();
                    if let Decision::Stop(reason) =
                        workflow::next_ready_node(&mut workflow, &task, MAX_RETRIES)
                    {
                        print_stop(reason);
                        task.terminal_reason = Some(reason);
                        if let Err(error) = task::save_current(&task) {
                            eprintln!("Error saving task: {error}");
                            return 1;
                        }
                        return stop_exit_code(reason);
                    }
                }
            }
            Err(error) => {
                task.workflow.mark_failed(&node_id);
                eprintln!("Error running agent step: {error}");
                return 1;
            }
        }
    }

    print_stop(StopReason::MaxSteps);
    task.terminal_reason = Some(StopReason::MaxSteps);
    if let Err(error) = task::save_current(&task) {
        eprintln!("Error saving task: {error}");
        return 1;
    }
    stop_exit_code(StopReason::MaxSteps)
}

fn run_step(_task_target: Option<TaskTarget>) -> i32 {
    let mut task = match task::load_current() {
        Ok(task) => task,
        Err(error) => {
            eprintln!("Error loading current task: {error}");
            return 1;
        }
    };

    let mut workflow = task.workflow.clone();
    let (node_id, next) = match workflow::next_ready_node(&mut workflow, &task, MAX_RETRIES) {
        Decision::Stop(reason) => {
            print_stop(reason);
            task.terminal_reason = Some(reason);
            if let Err(error) = task::save_current(&task) {
                eprintln!("Error saving task: {error}");
                return 1;
            }
            return stop_exit_code(reason);
        }
        Decision::Ready(id, op) => (id, op),
    };
    workflow.mark_running(&node_id);
    task.workflow = workflow;
    if let Err(error) = task::save_current(&task) {
        eprintln!("Error saving task: {error}");
        return 1;
    }

    let step_index = task.route.len() + 1;
    match execute_step(&next, &mut task, step_index) {
        Ok(outcome) => {
            task.workflow
                .complete(node_id, next, outcome.summary.clone());
            record_route(&mut task, &next, &outcome);
            println!("Selected step: {}", next.name());
            println!("Executor: {:?}", next.executor());
            println!("Observation: {}", outcome.summary);
            if let Err(error) = task::save_current(&task) {
                eprintln!("Error saving task: {error}");
                return 1;
            }
            0
        }
        Err(error) => {
            task.workflow.mark_failed(&node_id);
            eprintln!("Error running agent step: {error}");
            1
        }
    }
}

fn run_trace(_task_target: Option<TaskTarget>) -> i32 {
    let task = match task::load_current() {
        Ok(task) => task,
        Err(error) => {
            eprintln!("Error loading current task: {error}");
            return 1;
        }
    };

    println!("Route:");
    if task.route.is_empty() {
        println!("  (no steps recorded)");
    } else {
        for entry in &task.route {
            println!(
                "  {}({}) -> {}",
                entry.step,
                entry.executor.name(),
                entry.outcome
            );
        }
    }
    println!();

    println!("Graph:");
    if task.workflow.nodes.is_empty() {
        println!("  (no nodes)");
    } else {
        for node in &task.workflow.nodes {
            let deps = if node.depends_on.is_empty() {
                "-".to_string()
            } else {
                node.depends_on.join(",")
            };
            println!(
                "  {} [{}] deps={} status={:?}",
                node.id,
                node.op.name(),
                deps,
                node.status
            );
        }
    }
    println!();

    match store::agent_traces_for_task(Path::new(RUN_STORE_PATH), &task.id) {
        Ok(traces) => {
            if traces.is_empty() {
                println!("No agent traces recorded for current task.");
                return 0;
            }

            for trace in traces {
                println!("Step {}  {}", trace.step_index, trace.created_at);
                println!(
                    "Estimated tokens: input {} output {}",
                    trace.estimated_input_tokens, trace.estimated_output_tokens
                );
                println!(
                    "Reported tokens: input {} output {}",
                    format_optional(trace.reported_input_tokens),
                    format_optional(trace.reported_output_tokens)
                );
                println!("Action: {}", trace.action_json);
                println!("Observation: {}", trace.observation);
                println!(
                    "Selected context: planner system policy + TASK/ENVIRONMENT/CURRENT FAILURE/KNOWN CONTEXT/OPEN HYPOTHESES/BUDGET sections"
                );
                println!(
                    "Omitted context: raw stdout, raw stderr, full source files, previous trace text, full command history, tool docs"
                );
                println!("Prompt:\n{}", trace.prompt);
                println!("Response:\n{}", trace.response);
            }
            0
        }
        Err(error) => {
            eprintln!("Error loading agent trace: {error}");
            1
        }
    }
}

fn execute_step(
    step: &NodeOp,
    task: &mut task::TaskState,
    step_index: usize,
) -> io::Result<StepResult> {
    let primitive = primitive::primitive_for_node_op(step).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("no primitive registered for node operation {}", step.name()),
        )
    })?;
    if primitive.executor != step.executor() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "primitive {}@{} uses {:?}, but {} dispatches through {:?}",
                primitive.id,
                primitive.version,
                primitive.executor,
                step.name(),
                step.executor()
            ),
        ));
    }

    match step {
        NodeOp::ClassifyIntent => execute_classify_intent(task, step_index),
        NodeOp::DetectProject => execute_detect_project(task),
        NodeOp::ResolveVerification => execute_resolve_verification(task),
        NodeOp::RunBaseline => execute_run_baseline(task),
        NodeOp::ExtractEvidence => execute_extract_evidence(task),
        NodeOp::SelectContext => execute_select_context(task, step_index),
        NodeOp::PlanContext => execute_plan_context(task, step_index),
        NodeOp::ReadContext => execute_read_context(task),
        NodeOp::PlanPatch => execute_plan_patch(task, step_index),
        NodeOp::ApplyPatch => execute_apply_patch(task),
        NodeOp::RunFinalVerification => execute_run_final_verification(task),
        NodeOp::RetryFix => execute_retry_fix(task),
        NodeOp::AskUser => execute_ask_user(task),
        NodeOp::DirectAnswer => execute_direct_answer(task, step_index),
        NodeOp::Report => execute_report(task),
    }
}

fn print_stop(reason: StopReason) {
    match reason {
        StopReason::Verified => println!("Task verified."),
        StopReason::LoopDetected => println!("Stopped: same failure signature detected twice."),
        StopReason::BudgetExhausted => println!("Stopped: token budget exhausted."),
        StopReason::Blocked => println!("Stopped: blocked; needs user input."),
        StopReason::Failed => println!("Stopped: step failed."),
        StopReason::MaxSteps => println!("Stopped: max steps reached."),
    }
}

fn stop_exit_code(reason: StopReason) -> i32 {
    match reason {
        StopReason::Verified => 0,
        StopReason::LoopDetected
        | StopReason::BudgetExhausted
        | StopReason::Blocked
        | StopReason::Failed
        | StopReason::MaxSteps => 1,
    }
}

fn record_route(task: &mut task::TaskState, step: &NodeOp, outcome: &StepResult) {
    let primitive = primitive::primitive_for_node_op(step)
        .expect("execute_step requires every node operation to have a primitive");
    task.route.push(task::RouteEntry {
        step: step.name().to_string(),
        executor: step.executor(),
        primitive_id: Some(primitive.id.clone()),
        primitive_version: Some(primitive.version),
        outcome: outcome.summary.clone(),
    });
}

fn execute_classify_intent(
    task: &mut task::TaskState,
    step_index: usize,
) -> io::Result<StepResult> {
    let Some(weak_config) =
        Config::load_weak_model().map_err(|error| io::Error::other(error.to_string()))?
    else {
        let path_hint = crate::config::UserConfig::path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "~/.config/haycut/config.toml".to_string());
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("agent step requires [model] configuration in {path_hint}"),
        ));
    };
    let model_name = weak_config.model.clone();
    let billed = weak_config.billed;
    let weak = OpenAiProvider::new(weak_config);

    let (intent, input_tokens, output_tokens, response_text, manifest_id) =
        classify_task(&weak, task, step_index, &model_name, billed)?;
    task.intent = Some(intent);
    task.workflow_spec = Some(workflow_spec::compile_compatibility_spec(task));
    task.budget.packet_tokens_used = task
        .budget
        .packet_tokens_used
        .saturating_add(input_tokens)
        .saturating_add(output_tokens);

    let summary = format!("classified intent: {:?}", intent);
    record_agent_trace(
        task,
        step_index,
        AgentTraceInput {
            model: &model_name,
            purpose: "intent_classification",
            prompt: &summary,
            response: &response_text,
            executor: ExecutorKind::WeakModel,
            input_tokens,
            output_tokens,
            observation: &summary,
            billed,
            manifest_id: Some(&manifest_id),
        },
    )?;

    Ok(StepResult {
        summary,
        terminal: false,
    })
}

fn execute_detect_project(task: &mut task::TaskState) -> io::Result<StepResult> {
    let env = detect_project_env(Path::new("."));
    let summary = if let Some(env) = &env {
        task.project = Some(task::ProjectCard {
            language: env.language.clone(),
            test_command: env.test_command.clone(),
            build_command: env.build_command.clone(),
        });
        format!(
            "detected project: {} (test: `{}`)",
            env.language, env.test_command
        )
    } else {
        "unknown project type".to_string()
    };

    Ok(StepResult {
        summary,
        terminal: false,
    })
}

/// Build the structured verification plan for this task: explicit
/// `--verify`/`verify <command>` input becomes required, full-project
/// checks that always run; the project-detected test command is added too
/// (as required, unless the user already supplied their own commands, in
/// which case it's kept as an optional check so user intent still wins
/// without silently dropping the project default).
fn execute_resolve_verification(task: &mut task::TaskState) -> io::Result<StepResult> {
    let mut checks = Vec::new();
    let has_explicit = !task.explicit_verify_commands.is_empty();

    for raw in &task.explicit_verify_commands {
        if let Some(command) = task::VerificationCommand::parse(raw) {
            checks.push(task::VerificationCheck {
                command,
                required: true,
                scope: task::VerificationScope::FullProject,
            });
        }
    }

    let project = task.project.clone();
    if let Some(project) = &project
        && let Some(command) = task::VerificationCommand::parse(&project.test_command)
    {
        checks.push(task::VerificationCheck {
            command,
            required: !has_explicit,
            scope: task::VerificationScope::FullProject,
        });
    }

    if checks.is_empty() {
        return Ok(StepResult {
            summary: "no verification checks resolved (no project detected and no --verify given)"
                .to_string(),
            terminal: false,
        });
    }

    let expected_baseline_exit = match task.intent {
        Some(task::TaskIntent::DebugFailure) => Some(101),
        _ => None,
    };

    let summary = format!(
        "verification plan: {}",
        checks
            .iter()
            .map(|check| format!(
                "{}{}",
                check.command.display(),
                if check.required { "" } else { " (optional)" }
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );

    task.verification = Some(task::VerificationPlan {
        checks,
        expected_baseline_exit,
    });

    Ok(StepResult {
        summary,
        terminal: false,
    })
}

fn execute_run_baseline(task: &mut task::TaskState) -> io::Result<StepResult> {
    let Some(command) = task.verification.as_ref().and_then(|plan| plan.primary_command().cloned())
    else {
        return Ok(StepResult {
            summary: "no verification plan".to_string(),
            terminal: false,
        });
    };

    let exit_code = trace::run(command.as_vec(), None, Some(TaskTarget::Current));
    // trace::run persists evidence via attach_current_run; reload to see it.
    *task = task::load_current()?;

    let summary = format!("baseline `{}` exited {exit_code}", command.display());
    Ok(StepResult {
        summary,
        terminal: false,
    })
}

fn execute_extract_evidence(task: &mut task::TaskState) -> io::Result<StepResult> {
    let summary = if let Some(failure) = &task.current_failure {
        format!(
            "extracted {}: {} at {}",
            failure.kind,
            failure.summary,
            failure.locations.join(", ")
        )
    } else {
        "no current failure extracted".to_string()
    };

    Ok(StepResult {
        summary,
        terminal: false,
    })
}

/// Observation source tag left when the patch guard rejects a fix, so it fires
/// at most once per task and does not loop.
const PATCH_GUARD_SOURCE: &str = "agent:patch_guard";
/// Observation source tag for a body injected via the strong planner's `pull`
/// tool.
const PULLED_CONTEXT_SOURCE: &str = "agent:pulled_context";

/// Bound on how many hops the deterministic call-stack follow takes from the
/// failure site before giving up on a branch.
const MAX_CANDIDATE_DEPTH: usize = 3;
/// Bound on how many off-site candidates a single context-selection step
/// surfaces, keeping the listing (and the weak-gate prompt) small.
const MAX_CANDIDATES: usize = 5;
/// Aggregate token budget for candidate bodies rendered into the weak-model
/// relevance prompt, on top of the existing per-body 2_000-char cap — keeps
/// N candidates x body bounded instead of growing unbounded with N.
const CANDIDATE_LISTING_TOKEN_BUDGET: usize = 3_000;

/// A candidate off-site symbol resolved deterministically from the failure
/// site, before the weak-model relevance gate runs.
#[derive(Clone)]
struct OffSiteCandidate {
    id: String,
    symbol: String,
    path: String,
    start_line: usize,
    body: String,
}

/// Deterministic call-stack follow + weak-model relevance gate. Retrieval is
/// zero-token: starting from the failure site(s), it follows called symbols
/// (via `function_call_name`) across files (via `read_symbol`) up to
/// `MAX_CANDIDATE_DEPTH` hops, recursing into same-file calls until it lands
/// on a different file. Candidates are *not* pushed into the prompt as
/// observations — they are stored on `task.available_context` and only
/// listed by name/path to the strong planner, which pulls a body in only if
/// it decides it needs it. The weak model's only job here is a best-effort
/// yes/no relevance judgment per candidate; if it is unavailable, fails, or
/// returns no relevant ids at all (a degenerate, low-confidence result on an
/// already small, pre-filtered candidate set), every candidate is still
/// offered with `relevant: None` — never dropped.
fn execute_select_context(task: &mut task::TaskState, step_index: usize) -> io::Result<StepResult> {
    let Some(failure) = task.current_failure.clone() else {
        return Ok(StepResult {
            summary: "no failure to select context for".to_string(),
            terminal: false,
        });
    };

    let candidates = collect_graph_candidates(&failure);
    if candidates.is_empty() {
        return Ok(StepResult {
            summary: "call-stack follow surfaced no off-site candidates".to_string(),
            terminal: false,
        });
    }

    let listing = candidates
        .iter()
        .map(|candidate| format!("{}@{}", candidate.symbol, candidate.path))
        .collect::<Vec<_>>()
        .join(", ");
    for candidate in &candidates {
        let file_digest = std::fs::read(&candidate.path)
            .ok()
            .map(|bytes| crate::context::artifact::file_content_digest(&bytes));
        task.available_context.push(task::AvailableContext {
            id: candidate.id.clone(),
            symbol: candidate.symbol.clone(),
            path: candidate.path.clone(),
            start_line: candidate.start_line,
            body: candidate.body.clone(),
            file_digest,
            relevant: None,
        });
    }

    let (ranking_failure, ranking_candidates) =
        compiled_ranking_inputs(task, &failure, &candidates)?;
    let relevant_ids =
        judge_candidate_relevance(task, step_index, &ranking_failure, &ranking_candidates)?;
    for candidate in &mut task.available_context {
        if candidates.iter().any(|staged| staged.id == candidate.id) {
            candidate.relevant = relevant_ids.as_ref().map(|ids| ids.contains(&candidate.id));
        }
    }

    let eager_summary = maybe_eager_load_context(task);

    Ok(StepResult {
        summary: eager_summary.unwrap_or_else(|| {
            format!("surfaced available off-site context: {listing}")
        }),
        terminal: false,
    })
}

/// Bound on how many relevant candidates the eager-load gate will pull
/// deterministically before generating a patch. Reuses `MAX_CANDIDATES`'s
/// scale — a small, weak-model-confident set is cheap to just load rather
/// than hand back to a strong `plan_context` call to re-decide.
const EAGER_CONTEXT_MAX: usize = 3;

/// If `SelectContext` surfaced a small, confident, in-budget set of relevant
/// candidates, pull them all in immediately and queue `PlanPatch` — skipping
/// the strong `plan_context` hop that would otherwise just re-decide what the
/// weak ranking already confidently decided. Falls through to the normal
/// `PlanContext` path (by leaving `pending_agent_action` untouched) when the
/// set is large, low-confidence (`relevant` all `None`), or over budget.
fn maybe_eager_load_context(task: &mut task::TaskState) -> Option<String> {
    let relevant_ids: Vec<String> = task
        .available_context
        .iter()
        .filter(|candidate| candidate.relevant == Some(true))
        .map(|candidate| candidate.id.clone())
        .collect();

    if relevant_ids.is_empty() || relevant_ids.len() > EAGER_CONTEXT_MAX {
        return None;
    }

    let combined_tokens: usize = task
        .available_context
        .iter()
        .filter(|candidate| relevant_ids.contains(&candidate.id))
        .map(|candidate| estimate_tokens(candidate.body.as_bytes()))
        .sum();
    if combined_tokens > CANDIDATE_LISTING_TOKEN_BUDGET {
        return None;
    }

    let mut pulled = Vec::new();
    for id in &relevant_ids {
        if let Ok(result) = execute_pull_context(task, id) {
            pulled.push(result.summary);
        }
    }
    task.pending_agent_action = Some(AgentAction::PlanPatch);

    Some(format!(
        "eager-loaded confident context ({}); queued plan_patch",
        pulled.join(", ")
    ))
}

/// Build a `CodeGraph` over the workspace and traverse call edges from the
/// failure site(s): a same-file callee recurses into that callee's own
/// calls (as today); a cross-file callee is a candidate. Bounded by
/// `MAX_CANDIDATE_DEPTH` and `MAX_CANDIDATES`. Falls back to no candidates —
/// same as the previous empty-candidate path — if the graph fails to build
/// or a failure location doesn't resolve to an enclosing symbol, rather than
/// aborting the run.
fn collect_graph_candidates(failure: &task::CurrentFailure) -> Vec<OffSiteCandidate> {
    let Ok(graph) = code_graph::CodeGraph::build(Path::new(".")) else {
        return Vec::new();
    };

    let mut candidates = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for location in failure.locations.iter().take(3) {
        if candidates.len() >= MAX_CANDIDATES {
            break;
        }
        let Some((path, line)) = parse_location(location) else {
            continue;
        };

        let remaining = MAX_CANDIDATES - candidates.len();
        for found in graph.callees_from(&path, line, remaining, MAX_CANDIDATE_DEPTH) {
            if !seen.insert((found.symbol.clone(), found.path.clone())) {
                continue;
            }
            candidates.push(OffSiteCandidate {
                id: format!("c{}", candidates.len() + 1),
                symbol: found.symbol,
                path: found.path,
                start_line: found.start_line,
                body: truncate(&found.code, 2_000),
            });
            if candidates.len() >= MAX_CANDIDATES {
                break;
            }
        }
    }

    candidates
}

/// Ask the weak model a yes/no relevance judgment for each candidate. Returns
/// `Some(relevant_ids)` on success, `None` on any failure/unavailability or
/// when the model returns no relevant ids at all — the caller treats `None`
/// as "unknown" for every candidate, never as "drop them".
fn judge_candidate_relevance(
    task: &mut task::TaskState,
    step_index: usize,
    failure: &task::CurrentFailure,
    candidates: &[OffSiteCandidate],
) -> io::Result<Option<std::collections::HashSet<String>>> {
    let Some(weak_config) =
        Config::load_weak_model().map_err(|error| io::Error::other(error.to_string()))?
    else {
        return Ok(None);
    };
    let model_name = weak_config.model.clone();
    let billed = weak_config.billed;
    let weak = OpenAiProvider::new(weak_config);

    let known_ids: std::collections::HashSet<String> = candidates
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect();

    let mut remaining_budget = CANDIDATE_LISTING_TOKEN_BUDGET;
    let listing = candidates
        .iter()
        .map(|candidate| {
            let body_tokens = estimate_tokens(candidate.body.as_bytes());
            let body = if body_tokens > remaining_budget {
                eprintln!(
                    "select_context: trimming candidate {} body to fit aggregate token budget",
                    candidate.id
                );
                truncate(&candidate.body, remaining_budget.saturating_mul(4))
            } else {
                candidate.body.clone()
            };
            remaining_budget = remaining_budget.saturating_sub(body_tokens.min(remaining_budget));
            format!(
                "{}: {} @ {}\n```\n{}\n```",
                candidate.id, candidate.symbol, candidate.path, body
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let failure_site_frame = failure
        .locations
        .first()
        .and_then(|location| parse_location(location))
        .and_then(|(path, line)| {
            read_window::read_window(
                PathBuf::from(&path),
                line,
                read_window::DEFAULT_RADIUS,
                false,
            )
            .ok()
        })
        .map(|window| {
            let body = window
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "Failure site (where the failure surfaced / where the call originates — \
                 NOT an edit candidate, context only): {}\n```\n{body}\n```",
                window.path.display()
            )
        })
        .unwrap_or_default();

    let observed_detail = failure
        .detail
        .as_deref()
        .filter(|detail| !detail.is_empty())
        .unwrap_or(&failure.summary);

    let prompt = format!(
        "A test is failing. The observed assertion diff below (expected vs actual) is ground \
         truth about the bug's behavior. Treat test names, symbol names, and comments on any \
         candidate as unverified labels, not evidence — decide from the code's actual logic \
         and the observed diff which candidate's definition produces the wrong value. Every \
         candidate below was reached by deterministically following the call stack from the \
         failure site, so each is already known to be on the path — the question is not \
         whether it's reachable, but whether its own definition must be inspected or edited to \
         fix the bug at its source. Prefer the candidate that owns the business logic over one \
         that merely forwards a value along; the failure-site frame below is context showing \
         where the failure was observed, not an edit target. Return the ids of the candidates \
         whose definition is relevant.\n\n\
         Observed failure:\n```\n{observed_detail}\n```\n\n{failure_site_frame}\nCandidates:\n{listing}"
    );
    let tools = relevance_tools();
    let assembled = assemble_model_request(
        task,
        step_index,
        NodeOp::SelectContext,
        ModelPurpose::ContextRanking,
        None,
        &prompt,
        &tools,
        CLASSIFY_MAX_OUTPUT_TOKENS,
    )?;
    let input_estimate = assembled.request.estimated_tokens.input;

    // Best-effort gate: a weak model that fails to emit the tool call (common
    // for small local models) must degrade to "unknown relevance", never
    // abort the run.
    let Ok(invocation) = invocation::invoke_with_tools(
        Path::new(RUN_STORE_PATH),
        &weak,
        assembled,
        &tools,
        &model_name,
        billed,
        None,
    ) else {
        return Ok(None);
    };
    let manifest_id = invocation.manifest_id;
    let (_tool, args, response) = invocation.value;

    let relevant_ids = extract_relevant_ids(&args, &known_ids);

    let input = response.reported_tokens.input.unwrap_or(input_estimate);
    let output = response
        .reported_tokens
        .output
        .unwrap_or(CLASSIFY_MAX_OUTPUT_TOKENS);
    task.budget.packet_tokens_used = task
        .budget
        .packet_tokens_used
        .saturating_add(input)
        .saturating_add(output);
    let purpose = ModelPurpose::ContextRanking.to_string();
    let observation = format!(
        "relevant: {}",
        relevant_ids.iter().cloned().collect::<Vec<_>>().join(", ")
    );
    record_agent_trace(
        task,
        step_index,
        AgentTraceInput {
            model: &model_name,
            purpose: &purpose,
            prompt: &prompt,
            response: &response.text,
            executor: ExecutorKind::WeakModel,
            input_tokens: input,
            output_tokens: output,
            observation: &observation,
            billed,
            manifest_id: Some(&manifest_id),
        },
    )?;

    // An empty result from a small, already deterministically pre-filtered
    // candidate set is a low-confidence, degenerate outcome — not
    // meaningfully different from the model failing outright. Degrade to
    // "unknown" for every candidate rather than a confident "not relevant".
    if relevant_ids.is_empty() {
        return Ok(None);
    }

    Ok(Some(relevant_ids))
}

/// Tolerantly extract known candidate ids from a `judge_relevance` tool call.
/// Models occasionally emit ids under a different key than `relevant_ids`, or
/// echo back the full `"c1: symbol @ path"` label instead of the bare id.
/// Try likely keys in order, then normalize each returned string to its
/// leading `cN` token and keep it only if it names a real candidate.
fn extract_relevant_ids(
    args: &serde_json::Value,
    known_ids: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    for key in ["relevant_ids", "candidates", "ids", "relevant"] {
        let Some(items) = args.get(key).and_then(|value| value.as_array()) else {
            continue;
        };
        let ids: std::collections::HashSet<String> = items
            .iter()
            .filter_map(|value| value.as_str())
            .filter_map(|raw| {
                let token = raw
                    .split(|c: char| c == ':' || c.is_whitespace())
                    .next()
                    .unwrap_or(raw)
                    .trim();
                known_ids.contains(token).then(|| token.to_string())
            })
            .collect();
        if !ids.is_empty() {
            return ids;
        }
    }
    std::collections::HashSet::new()
}

/// Tool schema for the weak-model relevance gate: ids only, no code, no
/// enumeration of symbols (retrieval is deterministic).
fn relevance_tools() -> Vec<ToolDefinition> {
    primitive::context_ranker_profile().materialize(primitive::ToolProfileCapabilities::default())
}

/// Parse a failure location into `(path, line)`. Diagnostic extraction emits
/// either `path:line` or `path:line:column`; drop the optional column and keep
/// the line. Windows paths are not a concern — locations are always
/// `relative/path.rs:NN[:CC]`.
fn parse_location(location: &str) -> Option<(String, usize)> {
    let mut parts = location.rsplitn(3, ':');
    let last = parts.next()?;
    let middle = parts.next();
    let (path, line) = match (parts.next(), middle) {
        // `path:line:column` — `last` is the column, `middle` is the line.
        (Some(path), Some(line)) if last.trim().parse::<usize>().is_ok() => (path, line),
        // `path:line` — `middle` is the path, `last` is the line.
        (None, Some(path)) => (path, last),
        _ => return None,
    };
    let line: usize = line.trim().parse().ok()?;
    if path.is_empty() {
        return None;
    }
    Some((path.to_string(), line))
}

fn execute_plan_context(task: &mut task::TaskState, step_index: usize) -> io::Result<StepResult> {
    let Some(strong_config) =
        Config::load_strong_model().map_err(|error| io::Error::other(error.to_string()))?
    else {
        let path_hint = crate::config::UserConfig::path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "~/.config/haycut/config.toml".to_string());
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("agent step requires [model] configuration in {path_hint}"),
        ));
    };
    let model_name = strong_config.model.clone();
    let billed = strong_config.billed;
    let strong = OpenAiProvider::new(strong_config);

    let prompt = planner_prompt(task);
    let tools = planner_tools(task);
    let assembled = assemble_model_request(
        task,
        step_index,
        NodeOp::PlanContext,
        ModelPurpose::AgentPlanner,
        Some(PLANNER_SYSTEM_PROMPT),
        &prompt,
        &tools,
        max_output_tokens_for(ModelPurpose::AgentPlanner),
    )?;
    let estimated = assembled.request.estimated_tokens;
    let invocation = invocation::invoke_with_tools(
        Path::new(RUN_STORE_PATH),
        &strong,
        assembled,
        &tools,
        &model_name,
        billed,
        None,
    )?;
    let manifest_id = invocation.manifest_id;
    let (tool_name, args_value, response) = invocation.value;
    let action = action_from_tool_call(&tool_name, args_value)?;
    validate_action(&action, task)?;

    // Thread the planner's decision through as a typed AgentAction — never a
    // stringified command re-parsed later — so ReadContext (or the workflow
    // decision, for terminal actions) can act on it deterministically.
    task.pending_agent_action = Some(agent_action_from_planner_action(&action));

    let cost = estimated.input + response.reported_tokens.output.unwrap_or(estimated.output);
    task.budget.packet_tokens_used = task.budget.packet_tokens_used.saturating_add(cost);

    let action_json = serde_json::to_string(&action).map_err(io::Error::other)?;
    store::insert_agent_trace(
        Path::new(RUN_STORE_PATH),
        &NewAgentTrace {
            id: &trace_id(),
            task_id: &task.id,
            step_index: step_index as i64,
            model: &model_name,
            purpose: &ModelPurpose::AgentPlanner.to_string(),
            prompt: &prompt,
            response: &response.text,
            action_json: &action_json,
            observation: &action.reason,
            estimated_input_tokens: estimated.input as i64,
            estimated_output_tokens: estimated.output as i64,
            reported_input_tokens: response.reported_tokens.input.map(|value| value as i64),
            reported_output_tokens: response.reported_tokens.output.map(|value| value as i64),
            billed,
            manifest_id: Some(&manifest_id),
            created_at: &Utc::now().to_rfc3339(),
        },
    )?;

    Ok(StepResult {
        summary: format!("planner: {} — {}", action.action_name(), action.reason),
        terminal: action.action == ActionKind::Finish || action.action == ActionKind::AskUser,
    })
}

fn execute_read_context(task: &mut task::TaskState) -> io::Result<StepResult> {
    let Some(action) = task.pending_agent_action.clone() else {
        return Ok(StepResult {
            summary: "no queued context action".to_string(),
            terminal: false,
        });
    };

    if let AgentAction::PullContext { id } = &action {
        task.pending_agent_action = None;
        return execute_pull_context(task, id);
    }

    let observation = match &action {
        AgentAction::Search { query } => execute_search(query)?,
        AgentAction::ReadSymbol { target } => execute_read_symbol(task, target)?,
        AgentAction::ReadWindow { path, line, radius } => {
            execute_read_window(task, &path.to_string_lossy(), *line, *radius)?
        }
        AgentAction::RunCommand { program, args } => execute_run_command(task, program, args)?,
        // PlanPatch/AskUser/Finish are terminal decisions the workflow routes
        // away from ReadContext (see `decide()`); reaching here indicates a
        // routing bug rather than a normal outcome.
        AgentAction::PullContext { .. } => unreachable!("handled above"),
        AgentAction::PlanPatch | AgentAction::AskUser { .. } | AgentAction::Finish => {
            task.pending_agent_action = None;
            return Ok(StepResult {
                summary: "no-op: terminal action reached read_context".to_string(),
                terminal: false,
            });
        }
    };

    task.observations.push(task::Observation {
        id: format!("obs{}", task.observations.len() + 1),
        source: "agent:read_context".to_string(),
        kind: "agent_read_context".to_string(),
        summary: observation.clone(),
        locations: Vec::new(),
        tokens: task::ObservationTokens {
            raw: estimate_tokens(observation.as_bytes()),
            packet: estimate_tokens(observation.as_bytes()),
        },
    });
    task.pending_agent_action = None;

    Ok(StepResult {
        summary: observation,
        terminal: false,
    })
}

/// Deterministic injection for the `pull` tool: look up the candidate by id,
/// push its cached body as an observation, and drop it from
/// `available_context`. No re-scan, no model — the body was already read
/// during `execute_select_context`.
fn execute_pull_context(task: &mut task::TaskState, id: &str) -> io::Result<StepResult> {
    let Some(index) = task
        .available_context
        .iter()
        .position(|candidate| candidate.id == id)
    else {
        return Ok(StepResult {
            summary: format!("pull: no available context with id `{id}`"),
            terminal: false,
        });
    };
    let candidate = task.available_context.remove(index);

    let location = format!("{}:{}", candidate.path, candidate.start_line);
    let summary = format!(
        "{} (defined in {}):\n{}",
        candidate.symbol, location, candidate.body
    );
    task.observations.push(task::Observation {
        id: format!("obs{}", task.observations.len() + 1),
        source: PULLED_CONTEXT_SOURCE.to_string(),
        kind: "off_site_symbol".to_string(),
        summary: summary.clone(),
        locations: vec![location],
        tokens: task::ObservationTokens {
            raw: estimate_tokens(candidate.body.as_bytes()),
            packet: estimate_tokens(summary.as_bytes()),
        },
    });

    Ok(StepResult {
        summary: format!("pulled {}", candidate.symbol),
        terminal: false,
    })
}

fn execute_plan_patch(task: &mut task::TaskState, step_index: usize) -> io::Result<StepResult> {
    let Some(strong_config) =
        Config::load_strong_model().map_err(|error| io::Error::other(error.to_string()))?
    else {
        let path_hint = crate::config::UserConfig::path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "~/.config/haycut/config.toml".to_string());
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("agent step requires [model] configuration in {path_hint}"),
        ));
    };
    let model_name = strong_config.model.clone();
    let billed = strong_config.billed;
    let strong = OpenAiProvider::new(strong_config);

    let prompt = patch_plan_prompt(task);
    let tools = edit_tools();
    let assembled = assemble_model_request(
        task,
        step_index,
        NodeOp::PlanPatch,
        ModelPurpose::PatchGeneration,
        None,
        &prompt,
        &tools,
        max_output_tokens_for(ModelPurpose::PatchGeneration),
    )?;
    let estimated = assembled.request.estimated_tokens;
    let invocation = invocation::invoke_with_tools(
        Path::new(RUN_STORE_PATH),
        &strong,
        assembled,
        &tools,
        &model_name,
        billed,
        None,
    )?;
    let manifest_id = invocation.manifest_id;
    let (tool_name, args_value, response) = invocation.value;
    let edits = patch_edits_from_tool_call(&tool_name, args_value)?;
    let patch_text = render_patch_edits(&edits);
    task.patch_edits = Some(edits);
    task.patch_text = Some(patch_text.clone());

    let cost = estimated.input + response.reported_tokens.output.unwrap_or(estimated.output);
    task.budget.packet_tokens_used = task.budget.packet_tokens_used.saturating_add(cost);

    let action_json = format!("{{\"action\":\"plan_patch\",\"tool\":\"{tool_name}\"}}");
    store::insert_agent_trace(
        Path::new(RUN_STORE_PATH),
        &NewAgentTrace {
            id: &trace_id(),
            task_id: &task.id,
            step_index: step_index as i64,
            model: &model_name,
            purpose: &ModelPurpose::PatchGeneration.to_string(),
            prompt: &prompt,
            response: &response.text,
            action_json: &action_json,
            observation: &patch_text,
            estimated_input_tokens: estimated.input as i64,
            estimated_output_tokens: estimated.output as i64,
            reported_input_tokens: response.reported_tokens.input.map(|value| value as i64),
            reported_output_tokens: response.reported_tokens.output.map(|value| value as i64),
            billed,
            manifest_id: Some(&manifest_id),
            created_at: &Utc::now().to_rfc3339(),
        },
    )?;

    Ok(StepResult {
        summary: format!("patch plan: {}", first_line(&patch_text)),
        terminal: false,
    })
}

/// Tool schema for structured patch edits: a short list of exact find/replace
/// anchors instead of prose or a unified diff. This is what keeps patch
/// generation cheap — no hunk headers, no unchanged context lines, no
/// verification/summary boilerplate.
fn edit_tools() -> Vec<ToolDefinition> {
    primitive::patch_editor_profile().materialize(primitive::ToolProfileCapabilities::default())
}

/// Map the `propose_edits` tool call back to structured `PatchEdit`s.
fn patch_edits_from_tool_call(
    tool: &str,
    args: serde_json::Value,
) -> io::Result<Vec<task::PatchEdit>> {
    if tool != "propose_edits" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown tool `{tool}` returned by model"),
        ));
    }

    let edits = args
        .get("edits")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "propose_edits response missing `edits` array",
            )
        })?;

    edits.iter().map(patch_edit_from_json).collect()
}

fn patch_edit_from_json(edit: &serde_json::Value) -> io::Result<task::PatchEdit> {
    let str_field = |name: &str| {
        edit.get(name)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let digest_field = |name: &str| -> io::Result<patch::FileDigest> {
        edit.get(name)
            .and_then(|v| v.as_str())
            .map(|value| patch::FileDigest(value.to_string()))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("propose_edits: `{name}` is required for this edit kind"),
                )
            })
    };
    // Default to `replace` so the flat legacy shape (no `kind`) still maps.
    let kind = edit.get("kind").and_then(|v| v.as_str()).unwrap_or("replace");

    Ok(match kind {
        "replace" => task::PatchEdit::Replace {
            path: str_field("path"),
            find: str_field("find"),
            replace: str_field("replace"),
            expected_digest: edit
                .get("expected_digest")
                .and_then(|v| v.as_str())
                .map(|value| patch::FileDigest(value.to_string())),
        },
        "create" => task::PatchEdit::Create {
            path: str_field("path"),
            content: str_field("content"),
            overwrite: edit.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(false),
        },
        "delete" => task::PatchEdit::Delete {
            path: str_field("path"),
            expected_digest: digest_field("expected_digest")?,
        },
        "rename" => task::PatchEdit::Rename {
            from: str_field("from"),
            to: str_field("to"),
            expected_digest: digest_field("expected_digest")?,
        },
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("propose_edits: unknown edit kind `{other}`"),
            ));
        }
    })
}

/// Render structured edits into the compact, human/eval-facing `patch_text`.
/// Every edit kind is rendered so patch approval displays all planned file
/// operations, not just replacements.
fn render_patch_edits(edits: &[task::PatchEdit]) -> String {
    if edits.is_empty() {
        return "no edits proposed".to_string();
    }
    edits
        .iter()
        .map(|edit| match edit {
            task::PatchEdit::Replace { path, find, replace, .. } => {
                format!("replace {path}: \"{find}\" -> \"{replace}\"")
            }
            task::PatchEdit::Create { path, overwrite, .. } => {
                if *overwrite {
                    format!("create {path} (overwrite)")
                } else {
                    format!("create {path}")
                }
            }
            task::PatchEdit::Delete { path, .. } => format!("delete {path}"),
            task::PatchEdit::Rename { from, to, .. } => format!("rename {from} -> {to}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn execute_apply_patch(task: &mut task::TaskState) -> io::Result<StepResult> {
    // Wrong-fix guard: if context selection surfaced off-site helper
    // definitions the failure depends on, a fix that touches none of those
    // files is almost certainly reimplementing the helper's logic inline
    // (and orphaning it) rather than correcting the bug at its source. Reject
    // once and force a re-plan with the correction in context.
    if let Some(reason) = patch_scope_violation(task) {
        task.observations.push(task::Observation {
            id: format!("obs{}", task.observations.len() + 1),
            source: PATCH_GUARD_SOURCE.to_string(),
            kind: "patch_rejected".to_string(),
            summary: reason.clone(),
            locations: Vec::new(),
            tokens: task::ObservationTokens { raw: 0, packet: 0 },
        });
        task.patch_text = None;
        task.patch_edits = None;
        return Ok(StepResult {
            summary: format!("patch rejected: {reason}"),
            terminal: false,
        });
    }

    let root = patch::project_root()?;
    let outcome = match task.patch_edits.as_deref() {
        Some(edits) if !edits.is_empty() && task.apply_requested => {
            match patch::apply_edits(&root, edits, &task.inspected_digests) {
                Ok(summary) => Ok(summary),
                Err(error) if patch::is_conflict(&error) => Err(error),
                Err(error) => return Err(error),
            }
        }
        Some(edits) if !edits.is_empty() => {
            match patch::preview_edits(&root, edits, &task.inspected_digests) {
                Ok(preview) => Ok(format!("preview only; rerun with --apply to write:\n{preview}")),
                Err(error) if patch::is_conflict(&error) => Err(error),
                Err(error) => return Err(error),
            }
        }
        _ => Ok("no edits to apply".to_string()),
    };

    // Working-tree ownership conflicts (a file changed after the agent
    // inspected it) are recoverable: report them as a planner observation
    // and return to planning instead of failing the whole task.
    let summary = match outcome {
        Ok(summary) => summary,
        Err(conflict) => {
            let reason = conflict.to_string();
            task.observations.push(task::Observation {
                id: format!("obs{}", task.observations.len() + 1),
                source: "policy:working_tree_conflict".to_string(),
                kind: "working_tree_conflict".to_string(),
                summary: reason.clone(),
                locations: Vec::new(),
                tokens: task::ObservationTokens { raw: 0, packet: 0 },
            });
            task.patch_text = None;
            task.patch_edits = None;
            return Ok(StepResult {
                summary: format!("patch conflict: {reason}"),
                terminal: false,
            });
        }
    };

    task.patch_applied = task.apply_requested && !summary.starts_with("no edits");
    task.patch_previewed = !task.apply_requested && !summary.starts_with("no edits");
    Ok(StepResult {
        summary,
        terminal: false,
    })
}

/// Detect a patch that ignores every off-site symbol the failure depends on.
/// Returns `Some(reason)` when context selection resolved helper files but the
/// proposed edits touch none of them. Fires at most once per task: once a
/// `PATCH_GUARD_SOURCE` observation exists, the guard steps aside so a
/// deliberate caller-side fix is still reachable and the loop is bounded.
fn patch_scope_violation(task: &task::TaskState) -> Option<String> {
    if task
        .observations
        .iter()
        .any(|observation| observation.source == PATCH_GUARD_SOURCE)
    {
        return None;
    }

    // Off-site files the weak gate marked relevant but the planner never
    // pulled (still sitting in `available_context`).
    let helper_files: std::collections::BTreeSet<&str> = task
        .available_context
        .iter()
        .filter(|candidate| candidate.relevant == Some(true))
        .map(|candidate| candidate.path.as_str())
        .collect();
    if helper_files.is_empty() {
        return None;
    }

    let edits = task.patch_edits.as_deref()?;
    if edits.is_empty() {
        return None;
    }
    let touched: std::collections::BTreeSet<&str> =
        edits.iter().map(|edit| edit.primary_path()).collect();

    if touched.is_disjoint(&helper_files) {
        Some(format!(
            "fix edits {} but the failure delegates to {}; correct the bug at its source there instead of reimplementing it inline",
            touched.into_iter().collect::<Vec<_>>().join(", "),
            helper_files.into_iter().collect::<Vec<_>>().join(", "),
        ))
    } else {
        None
    }
}

/// Run every check in the resolved verification plan, in order, and report
/// required and optional outcomes separately so the final report/approval
/// can distinguish a broken required check (task not actually done) from a
/// failing optional one (worth a note, not a blocker).
fn execute_run_final_verification(task: &mut task::TaskState) -> io::Result<StepResult> {
    let Some(plan) = task.verification.clone() else {
        return Ok(StepResult {
            summary: "no verification plan".to_string(),
            terminal: false,
        });
    };
    if plan.checks.is_empty() {
        return Ok(StepResult {
            summary: "no verification checks to run".to_string(),
            terminal: false,
        });
    }

    let mut required_failures = Vec::new();
    let mut lines = Vec::with_capacity(plan.checks.len());
    let mut results = Vec::with_capacity(plan.checks.len());

    for check in &plan.checks {
        let exit_code = trace::run(check.command.as_vec(), None, Some(TaskTarget::Current));
        *task = task::load_current()?;

        let passed = exit_code == 0;
        let label = if check.required { "required" } else { "optional" };
        lines.push(format!(
            "{label} `{}` exited {exit_code} ({})",
            check.command.display(),
            if passed { "pass" } else { "fail" }
        ));
        if !passed && check.required {
            required_failures.push(check.command.display());
        }
        results.push(task::VerificationCheckResult {
            command: check.command.clone(),
            required: check.required,
            scope: check.scope,
            exit_code,
            passed,
        });
    }

    // Set after the loop: `*task = task::load_current()?;` above reloads the
    // task from disk on every iteration (to see evidence `trace::run`
    // persisted), which would otherwise wipe an in-progress result list set
    // mid-loop.
    task.verification_results = results;

    let overall = if required_failures.is_empty() { "pass" } else { "fail" };
    let summary = format!("final verification ({overall}): {}", lines.join("; "));

    Ok(StepResult {
        summary,
        terminal: false,
    })
}

fn execute_retry_fix(task: &mut task::TaskState) -> io::Result<StepResult> {
    task.last_failure_signature = task
        .current_failure
        .as_ref()
        .map(workflow::failure_signature);
    task.retry_count += 1;
    task.patch_text = None;
    task.patch_edits = None;
    task.patch_applied = false;
    task.next_actions.clear();
    task.pending_agent_action = None;

    Ok(StepResult {
        summary: format!("retry fix attempt {}", task.retry_count),
        terminal: false,
    })
}

fn execute_ask_user(task: &mut task::TaskState) -> io::Result<StepResult> {
    let summary = match &task.pending_agent_action {
        Some(AgentAction::AskUser { question }) => format!("ask user: {question}"),
        _ => "ask user".to_string(),
    };
    Ok(StepResult {
        summary,
        terminal: true,
    })
}

fn execute_direct_answer(task: &mut task::TaskState, step_index: usize) -> io::Result<StepResult> {
    let Some(strong_config) =
        Config::load_strong_model().map_err(|error| io::Error::other(error.to_string()))?
    else {
        let path_hint = crate::config::UserConfig::path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "~/.config/haycut/config.toml".to_string());
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("agent step requires [model] configuration in {path_hint}"),
        ));
    };
    let model_name = strong_config.model.clone();
    let billed = strong_config.billed;
    let strong = OpenAiProvider::new(strong_config);

    let prompt = format!(
        "Answer the following software task concisely.\n\nTask: {}\n\nKnown context:\n{}\n",
        task.goal,
        task.observations
            .iter()
            .map(|observation| format!("- {}", observation.summary))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let assembled = assemble_model_request(
        task,
        step_index,
        NodeOp::DirectAnswer,
        ModelPurpose::FinalReport,
        None,
        &prompt,
        &[],
        max_output_tokens_for(ModelPurpose::FinalReport),
    )?;
    let estimated = assembled.request.estimated_tokens;
    let invocation = invocation::invoke_plain(
        Path::new(RUN_STORE_PATH),
        &strong,
        assembled,
        &model_name,
        billed,
        None,
    )?;
    let manifest_id = invocation.manifest_id;
    let response = invocation.value;

    let cost = estimated.input + response.reported_tokens.output.unwrap_or(estimated.output);
    task.budget.packet_tokens_used = task.budget.packet_tokens_used.saturating_add(cost);

    let answer = response.text.trim().to_string();
    store::insert_agent_trace(
        Path::new(RUN_STORE_PATH),
        &NewAgentTrace {
            id: &trace_id(),
            task_id: &task.id,
            step_index: step_index as i64,
            model: &model_name,
            purpose: &ModelPurpose::FinalReport.to_string(),
            prompt: &prompt,
            response: &answer,
            action_json: "{\"action\":\"direct_answer\"}",
            observation: &answer,
            estimated_input_tokens: estimated.input as i64,
            estimated_output_tokens: estimated.output as i64,
            reported_input_tokens: response.reported_tokens.input.map(|value| value as i64),
            reported_output_tokens: response.reported_tokens.output.map(|value| value as i64),
            billed,
            manifest_id: Some(&manifest_id),
            created_at: &Utc::now().to_rfc3339(),
        },
    )?;

    Ok(StepResult {
        summary: answer,
        terminal: false,
    })
}

fn execute_report(task: &mut task::TaskState) -> io::Result<StepResult> {
    let summary = if task.patch_applied {
        if let Some(patch) = &task.patch_text {
            format!("planned patch:\n{}", patch)
        } else {
            "no patch planned".to_string()
        }
    } else {
        "task complete".to_string()
    };

    Ok(StepResult {
        summary,
        terminal: true,
    })
}

fn record_agent_trace(
    task: &task::TaskState,
    step_index: usize,
    input: AgentTraceInput<'_>,
) -> io::Result<()> {
    store::insert_agent_trace(
        Path::new(RUN_STORE_PATH),
        &NewAgentTrace {
            id: &trace_id(),
            task_id: &task.id,
            step_index: step_index as i64,
            model: input.model,
            purpose: input.purpose,
            prompt: input.prompt,
            response: input.response,
            // Key the action on its purpose, not just the executor tier: two
            // distinct weak-model steps (e.g. intent classification and context
            // selection) must not read as the same action to duplicate-action
            // detection.
            action_json: &format!(
                "{{\"step\":\"{}\",\"purpose\":\"{}\"}}",
                input.executor.name(),
                input.purpose,
            ),
            observation: input.observation,
            estimated_input_tokens: input.input_tokens as i64,
            estimated_output_tokens: input.output_tokens as i64,
            reported_input_tokens: Some(input.input_tokens as i64),
            reported_output_tokens: Some(input.output_tokens as i64),
            billed: input.billed,
            manifest_id: input.manifest_id,
            created_at: &Utc::now().to_rfc3339(),
        },
    )
}

/// Cheap, single-purpose classifier. Sends only the goal and a small enum of
/// intents to the weak model — no tool schemas, no history — so it stays a
/// low-cost classification call rather than a full planner turn.
/// Output budget for the classifier tool call. Some chat templates (e.g.
/// Qwen2.5 served via Ollama) emit a few tokens of preamble before the tool
/// call itself, so this needs headroom beyond the ~4 tokens a bare
/// `{"intent":"..."}` argument takes on models that respond with no preamble.
const CLASSIFY_MAX_OUTPUT_TOKENS: usize = 64;

fn classify_task(
    provider: &OpenAiProvider,
    task: &task::TaskState,
    step_index: usize,
    model_name: &str,
    billed: bool,
) -> io::Result<(task::TaskIntent, usize, usize, String, String)> {
    let goal = compiled_task_goal(task)?.unwrap_or_else(|| task.goal.clone());
    let prompt = format!("Classify task intent.\nTask: {goal}");
    let tools = classifier_tools();
    let assembled = assemble_model_request(
        task,
        step_index,
        NodeOp::ClassifyIntent,
        ModelPurpose::IntentClassification,
        None,
        &prompt,
        &tools,
        CLASSIFY_MAX_OUTPUT_TOKENS,
    )?;
    let input_estimate = assembled.request.estimated_tokens.input;

    let invocation = invocation::invoke_with_tools(
        Path::new(RUN_STORE_PATH),
        provider,
        assembled,
        &tools,
        model_name,
        billed,
        None,
    )?;
    let manifest_id = invocation.manifest_id;
    let (_tool, args, response) = invocation.value;

    let intent = parse_intent(args.get("intent").and_then(|value| value.as_str()));
    let input = response.reported_tokens.input.unwrap_or(input_estimate);
    let output = response
        .reported_tokens
        .output
        .unwrap_or(CLASSIFY_MAX_OUTPUT_TOKENS);
    Ok((intent, input, output, response.text, manifest_id))
}

fn compiled_task_goal(task: &task::TaskState) -> io::Result<Option<String>> {
    let Some(compiled) = compiled_context_for(task, NodeOp::ClassifyIntent)? else {
        return Ok(None);
    };
    compiled_task_goal_value(compiled).map(Some)
}

fn compiled_task_goal_value(
    compiled: crate::context::compiler::CompiledContext,
) -> io::Result<String> {
    compiled
        .selected_artifacts
        .into_iter()
        .find(|artifact| artifact.category == primitive::ContextCategory::TaskGoal)
        .map(|artifact| artifact.content)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "compiled classify_intent context is missing task_goal",
            )
        })
}

fn compiled_ranking_inputs(
    task: &task::TaskState,
    legacy_failure: &task::CurrentFailure,
    legacy_candidates: &[OffSiteCandidate],
) -> io::Result<(task::CurrentFailure, Vec<OffSiteCandidate>)> {
    let Some(compiled) = compiled_context_for(task, NodeOp::SelectContext)? else {
        return Ok((legacy_failure.clone(), legacy_candidates.to_vec()));
    };
    compiled_ranking_values(compiled)
}

fn compiled_ranking_values(
    compiled: crate::context::compiler::CompiledContext,
) -> io::Result<(task::CurrentFailure, Vec<OffSiteCandidate>)> {
    let failure = compiled
        .selected_artifacts
        .iter()
        .find(|artifact| artifact.category == primitive::ContextCategory::CurrentFailure)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "compiled select_context context is missing current_failure",
            )
        })
        .and_then(|artifact| serde_json::from_str(&artifact.content).map_err(io::Error::other))?;
    let mut candidates = compiled
        .selected_artifacts
        .into_iter()
        .filter(|artifact| artifact.category == primitive::ContextCategory::CodeGraphCandidate)
        .map(|artifact| {
            let value = |key: &str| {
                artifact.provenance.get(key).cloned().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("compiled code-graph candidate is missing {key} provenance"),
                    )
                })
            };
            Ok(OffSiteCandidate {
                id: value("candidate_id")?,
                symbol: value("symbol")?,
                path: value("path")?,
                start_line: value("start_line")?.parse().map_err(io::Error::other)?,
                body: artifact.content,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    candidates.sort_by(|left, right| left.id.cmp(&right.id));
    Ok((failure, candidates))
}

fn compiled_context_for(
    task: &task::TaskState,
    op: NodeOp,
) -> io::Result<Option<crate::context::compiler::CompiledContext>> {
    let context_config = Config::load_from_current_dir()
        .map(|config| config.context)
        .unwrap_or_default();
    let primitive = primitive::primitive_for_node_op(&op)
        .expect("compiled context requires a registered primitive");
    compiled_context_for_config(task, primitive, Path::new("."), &context_config)
}

fn compiled_context_for_config(
    task: &task::TaskState,
    primitive: &primitive::PrimitiveSpec,
    repository_root: &Path,
    context_config: &crate::config::ContextConfig,
) -> io::Result<Option<crate::context::compiler::CompiledContext>> {
    if !context_config.compiled_for(primitive.id.as_str()) {
        return Ok(None);
    }
    let compiled = crate::context::compiler::compile(
        task,
        primitive,
        repository_root,
        task.budget
            .hard_tokens
            .saturating_sub(task.budget.packet_tokens_used),
    )?;
    if !compiled.unresolved_requirements.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "compiled {} context has unresolved requirements: {:?}",
                primitive.id, compiled.unresolved_requirements
            ),
        ));
    }
    Ok(Some(compiled))
}

fn parse_intent(label: Option<&str>) -> task::TaskIntent {
    match label {
        Some("debug_failure") => task::TaskIntent::DebugFailure,
        Some("implement_feature") => task::TaskIntent::ImplementFeature,
        Some("refactor") => task::TaskIntent::Refactor,
        // Unknown/missing labels default to the least-destructive intent, which
        // never triggers a deterministic command run.
        _ => task::TaskIntent::AnswerQuestion,
    }
}

fn classifier_tools() -> Vec<ToolDefinition> {
    primitive::intent_classifier_profile()
        .materialize(primitive::ToolProfileCapabilities::default())
}

fn planner_tools(task: &task::TaskState) -> Vec<ToolDefinition> {
    primitive::context_planner_profile().materialize(primitive::ToolProfileCapabilities {
        pull_available_context: !task.available_context.is_empty(),
    })
}

/// Map a tool-call result back to the internal `PlannerAction` representation.
fn action_from_tool_call(tool: &str, args: serde_json::Value) -> io::Result<PlannerAction> {
    let str_field = |key: &str| -> Option<String> {
        args.get(key).and_then(|v| v.as_str()).map(str::to_string)
    };
    let usize_field =
        |key: &str| -> Option<usize> { args.get(key).and_then(|v| v.as_u64()).map(|n| n as usize) };
    let strvec_field = |key: &str| -> Vec<String> {
        args.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };

    let reason = str_field("reason").unwrap_or_default();

    let (kind, action_args) = match tool {
        "search" => (
            ActionKind::Search,
            ActionArgs {
                query: str_field("query"),
                ..Default::default()
            },
        ),
        "sym" => (
            ActionKind::ReadSymbol,
            ActionArgs {
                symbol: str_field("symbol"),
                ..Default::default()
            },
        ),
        "win" => (
            ActionKind::ReadWindow,
            ActionArgs {
                file: str_field("file"),
                line: usize_field("line"),
                radius: usize_field("radius"),
                ..Default::default()
            },
        ),
        "trace" => (
            ActionKind::Trace,
            ActionArgs {
                command: str_field("command"),
                args: strvec_field("args"),
                ..Default::default()
            },
        ),
        "plan" => (ActionKind::ProposePatchPlan, ActionArgs::default()),
        "finish" => (ActionKind::Finish, ActionArgs::default()),
        "ask" => (
            ActionKind::AskUser,
            ActionArgs {
                question: str_field("question"),
                ..Default::default()
            },
        ),
        "pull" => (
            ActionKind::PullContext,
            ActionArgs {
                id: str_field("id"),
                ..Default::default()
            },
        ),
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown tool `{other}` returned by model"),
            ));
        }
    };

    Ok(PlannerAction {
        action: kind,
        args: action_args,
        reason,
    })
}

/// Compiled Jinja environment holding the planner templates. Built once on
/// first use; the templates are embedded in the binary via `include_str!`.
fn planner_templates() -> &'static Environment<'static> {
    static ENV: OnceLock<Environment<'static>> = OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        // Match Jinja2's block-trimming so the template reads cleanly without
        // block tags leaving behind stray blank lines or indentation.
        env.set_trim_blocks(true);
        env.set_lstrip_blocks(true);
        env.set_keep_trailing_newline(true);
        env.add_template("planner_user", PLANNER_USER_TEMPLATE)
            .expect("planner user template must compile");
        env
    })
}

fn planner_prompt(task: &task::TaskState) -> String {
    // Only the last few observations and open hypotheses carry signal, so we
    // slice them here before handing the template a ready-to-render context.
    let observations: Vec<_> = task.observations.iter().rev().take(6).rev().collect();
    let hypotheses: Vec<_> = task
        .hypotheses
        .iter()
        .filter(|hypothesis| hypothesis.status == "open")
        .take(5)
        .collect();

    planner_templates()
        .get_template("planner_user")
        .expect("planner user template is registered")
        .render(context! {
            goal => task.goal,
            acceptance => task.acceptance,
            constraints => task.constraints,
            environment => task.project,
            failure => task.current_failure,
            observations => observations,
            hypotheses => hypotheses,
            available_context => task.available_context,
            budget => context! {
                used => task.budget.packet_tokens_used,
                soft => task.budget.soft_tokens,
                hard => task.budget.hard_tokens,
            },
        })
        .expect("planner prompt renders")
}

/// Output-token cap per purpose. Patch generation answers via a compact
/// structured tool call (no prose, no diff hunks), so it needs a much smaller
/// budget than free-form planning/reporting calls.
fn max_output_tokens_for(purpose: ModelPurpose) -> usize {
    match purpose {
        ModelPurpose::PatchGeneration => PATCH_MAX_OUTPUT_TOKENS,
        _ => MAX_OUTPUT_TOKENS,
    }
}

/// Reasoning-capable models (gpt-5 family, Claude) spend most of their output
/// budget on hidden reasoning tokens. These calls are structured and
/// near-deterministic, so the lowest supported effort keeps answer quality
/// while slashing reported output tokens. The provider ignores this hint for
/// models that don't support it.
///
/// Note: gpt-5-mini only accepts `low`/`medium`/`high` (not `minimal`), and
/// litellm maps the same tiers onto Claude's thinking-token budget, so `low`
/// is the safe floor across both.
fn reasoning_effort_for(purpose: ModelPurpose) -> Option<&'static str> {
    match purpose {
        ModelPurpose::IntentClassification | ModelPurpose::PatchGeneration => Some("low"),
        ModelPurpose::AgentPlanner | ModelPurpose::FinalReport => Some("low"),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn assemble_model_request(
    task: &task::TaskState,
    step_index: usize,
    node_op: NodeOp,
    purpose: ModelPurpose,
    system: Option<&str>,
    prompt: &str,
    tools: &[ToolDefinition],
    max_output_tokens: usize,
) -> io::Result<AssembledRequest> {
    let primitive = primitive::primitive_for_node_op(&node_op).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("no primitive registered for {}", node_op.name()),
        )
    })?;
    let mut metadata = std::collections::BTreeMap::new();
    metadata.insert("task_id".to_string(), task.id.clone());
    let producer_id = primitive.id.as_str();
    let producer_version = primitive.version.get();
    let system_segments = system
        .map(|content| {
            vec![ContextSegment::new(
                "system",
                0,
                ContextRole::System,
                primitive::ContextCategory::Constraints,
                ContextRepresentation::Raw,
                producer_id,
                producer_version,
                content,
                CachePolicy::Request,
            )]
        })
        .unwrap_or_default();
    let user_segments = vec![ContextSegment::new(
        "prompt",
        system_segments.len(),
        ContextRole::Task,
        primitive::ContextCategory::TaskGoal,
        ContextRepresentation::Generated,
        producer_id,
        producer_version,
        prompt,
        CachePolicy::NoStore,
    )];
    let correlation = RequestCorrelation {
        task_id: task.id.clone(),
        step_index,
        node_id: task
            .workflow
            .nodes
            .iter()
            .find(|node| node.status == workflow::NodeStatus::Running)
            .map(|node| node.id.clone()),
        workflow_compiler_version: task
            .workflow_spec
            .as_ref()
            .map(|spec| spec.compiler_version.clone()),
    };
    let mut assembled = request::assemble(RequestAssembly {
        primitive,
        system_segments,
        user_segments,
        tools,
        purpose,
        max_output_tokens,
        reasoning_effort: reasoning_effort_for(purpose).map(str::to_string),
        correlation,
        metadata,
    })
    .map_err(io::Error::other)?;

    let context_config = Config::load_from_current_dir()
        .map(|config| config.context)
        .unwrap_or_default();
    attach_context_comparison(
        task,
        primitive,
        Path::new("."),
        &context_config,
        &mut assembled,
    )?;

    Ok(assembled)
}

fn attach_context_comparison(
    task: &task::TaskState,
    primitive: &primitive::PrimitiveSpec,
    repository_root: &Path,
    context_config: &crate::config::ContextConfig,
    assembled: &mut AssembledRequest,
) -> io::Result<()> {
    if context_config.compiler_mode != crate::config::CompilerMode::Off {
        let started = std::time::Instant::now();
        let required_categories: Vec<_> = primitive
            .context_requirements
            .iter()
            .filter(|requirement| {
                requirement.kind == crate::context::artifact::RequirementKind::Required
            })
            .map(|requirement| requirement.category)
            .collect();
        let mut comparison = match crate::context::compiler::compile(
            task,
            primitive,
            repository_root,
            task.budget
                .hard_tokens
                .saturating_sub(task.budget.packet_tokens_used),
        ) {
            Ok(compiled) => crate::context::comparison::compare(
                &required_categories,
                &assembled.manifest.segments,
                &compiled,
                started.elapsed(),
            ),
            Err(error) => crate::context::comparison::compiler_error(
                &required_categories,
                &assembled.manifest.segments,
                started.elapsed(),
                &error.to_string(),
            ),
        };
        if context_config.compiled_for(primitive.id.as_str())
            && matches!(primitive.id.as_str(), "classify_intent" | "select_context")
            && comparison.verdict == crate::context::comparison::ComparisonVerdict::Pass
        {
            comparison.authoritative = true;
        }
        assembled.manifest.comparison_json =
            Some(serde_json::to_string(&comparison).map_err(io::Error::other)?);
    }
    Ok(())
}

fn patch_plan_prompt(task: &task::TaskState) -> String {
    let observations: Vec<_> = task.observations.iter().rev().take(8).rev().collect();
    let mut prompt = String::from(
        "Fix the failure with the minimal set of exact edits. Fix the bug at its source: \
         if the failing code delegates to a helper shown in context, edit that helper — \
         do not reimplement its logic inline in the caller.\n",
    );
    // For DebugFailure tasks the goal is generic boilerplate ("test suite
    // fails, find the cause") that the instruction sentence already implies;
    // `current_failure` carries the concrete signal instead. Keep the goal for
    // every other intent, where it's the only description of what to do.
    let goal_is_redundant =
        task.current_failure.is_some() && task.intent == Some(task::TaskIntent::DebugFailure);
    if !goal_is_redundant {
        prompt.push_str(&format!("Goal: {}\n", task.goal));
    }
    let failure_summary = task.current_failure.as_ref().map(|failure| {
        prompt.push_str(&format!(
            "Current failure: {} — {}\n",
            failure.kind, failure.summary
        ));
        // The summary is often just a test name/label; the assertion diff in
        // `detail` (expected vs actual) is the actual ground truth about the
        // bug's behavior — without it the model has to guess intent from
        // labels alone, the same failure mode the context-ranking prompt
        // warns the weak model away from.
        if let Some(detail) = failure.detail.as_deref().filter(|d| !d.is_empty()) {
            prompt.push_str(&format!("Observed failure detail:\n```\n{detail}\n```\n"));
        }
        failure.summary.as_str()
    });
    let observations: Vec<_> = observations
        .into_iter()
        .filter(|observation| Some(observation.summary.as_str()) != failure_summary)
        .collect();
    if !observations.is_empty() {
        prompt.push_str("\nKnown context:\n");
        for observation in observations {
            prompt.push_str(&format!(
                "- {}\n",
                compact_observation(&observation.summary)
            ));
        }
    }
    prompt
}

/// Strip diagnostic scaffolding (line-number gutters, token/line metadata)
/// from a `FileWindow::render` string embedded in an observation summary.
/// `propose_edits` matches code verbatim, so gutters only add noise.
fn compact_observation(summary: &str) -> String {
    summary
        .lines()
        .filter(|line| !line.starts_with("Estimated tokens:") && !line.starts_with("Lines: "))
        .map(|line| {
            let trimmed = line.trim_start();
            match trimmed.split_once(" | ") {
                Some((gutter, code)) if gutter.trim().parse::<usize>().is_ok() => code,
                _ => line,
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Convert a planner action into the typed `AgentAction` threaded through the
/// workflow — no intermediate command string, so paths with spaces and
/// command arguments containing quotes are represented correctly.
fn agent_action_from_planner_action(action: &PlannerAction) -> AgentAction {
    match action.action {
        ActionKind::Search => AgentAction::Search {
            query: action.args.query.clone().unwrap_or_default(),
        },
        ActionKind::ReadSymbol => AgentAction::ReadSymbol {
            target: action.args.symbol.clone().unwrap_or_default(),
        },
        ActionKind::ReadWindow => AgentAction::ReadWindow {
            path: PathBuf::from(action.args.file.clone().unwrap_or_default()),
            line: action.args.line.unwrap_or(1),
            radius: action.args.radius.unwrap_or(read_window::DEFAULT_RADIUS),
        },
        ActionKind::Trace => AgentAction::RunCommand {
            program: action.args.command.clone().unwrap_or_default(),
            args: action.args.args.clone(),
        },
        ActionKind::PullContext => AgentAction::PullContext {
            id: action.args.id.clone().unwrap_or_default(),
        },
        ActionKind::ProposePatchPlan => AgentAction::PlanPatch,
        ActionKind::Finish => AgentAction::Finish,
        ActionKind::AskUser => AgentAction::AskUser {
            question: action.args.question.clone().unwrap_or_default(),
        },
    }
}

/// Run an arbitrary command the planner asked to `trace`, gated by command
/// risk policy (Phase 7 of `plan_3_safety_and_execution.md`): low-risk
/// commands (tests, builds, read-only Git, ...) auto-run under a timeout;
/// medium-risk commands (package installs, codegen, commands that touch
/// tracked files, ...) require the same `--apply` authorization a patch
/// write needs; high-risk commands (destructive filesystem/Git ops, network
/// publishing, credential access) are denied outright. Denials and
/// approval-required commands are never executed — they come back as a
/// planner-visible observation describing why, not a hard error.
fn execute_run_command(task: &task::TaskState, program: &str, args: &[String]) -> io::Result<String> {
    let risk = command_policy::classify(program, args);
    let command_display = format!("{program} {}", args.join(" "));

    if risk == command_policy::RiskTier::High {
        return Ok(format!(
            "command policy: denied `{}` — classified high risk (destructive or credential-sensitive); not executed",
            command_display.trim()
        ));
    }
    if risk == command_policy::RiskTier::Medium && !task.apply_requested {
        return Ok(format!(
            "command policy: `{}` classified medium risk and requires approval; rerun the agent with --apply to authorize, or run it manually",
            command_display.trim()
        ));
    }

    let cwd = patch::project_root().unwrap_or_else(|_| PathBuf::from("."));
    let timeout = command_policy::timeout_for(risk);
    let outcome = command_policy::run_with_timeout(program, args, &cwd, timeout)?;
    let approved = match risk {
        command_policy::RiskTier::Low => "auto (low risk)",
        command_policy::RiskTier::Medium => "user (--apply)",
        command_policy::RiskTier::High => unreachable!("high risk denied above"),
    };
    let timeout_note = if outcome.timed_out {
        format!(" [timed out after {}ms, process killed]", timeout.as_millis())
    } else {
        String::new()
    };

    Ok(format!(
        "ran `{}` in {} — exit={} duration={}ms approved={approved}{timeout_note}\nstdout: {}\nstderr: {}",
        command_display.trim(),
        cwd.display(),
        outcome
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "none".to_string()),
        outcome.duration.as_millis(),
        truncate(&outcome.stdout, 400),
        truncate(&outcome.stderr, 400),
    ))
}

/// A deterministic, cheap description of the project's toolchain. HayCut
/// detects this from marker files instead of paying a model call (or a user
/// round-trip) to learn how the repo is built and tested.
#[derive(Serialize)]
struct ProjectEnv {
    language: String,
    test_command: String,
    build_command: Option<String>,
}

/// Detect the project environment from well-known marker files in `root`.
/// Returns `None` when no known ecosystem is recognised.
fn detect_project_env(root: &Path) -> Option<ProjectEnv> {
    let exists = |name: &str| root.join(name).exists();

    if exists("Cargo.toml") {
        return Some(ProjectEnv {
            language: "Rust (cargo)".to_string(),
            test_command: "cargo test".to_string(),
            build_command: Some("cargo build".to_string()),
        });
    }
    if exists("package.json") {
        return Some(detect_node_env(root));
    }
    if exists("pyproject.toml") || exists("pytest.ini") || exists("setup.cfg") || exists("setup.py")
    {
        return Some(detect_python_env(root));
    }
    if exists("go.mod") {
        return Some(ProjectEnv {
            language: "Go".to_string(),
            test_command: "go test ./...".to_string(),
            build_command: Some("go build ./...".to_string()),
        });
    }

    None
}

/// Pick a Python test runner based on the dependency manager in use. Presence
/// of a lock file is the strongest signal; `pyproject.toml` tables are a cheap
/// fallback. Running through the manager (`uv run` / `poetry run`) guarantees
/// the project's virtualenv is used.
fn detect_python_env(root: &Path) -> ProjectEnv {
    let exists = |name: &str| root.join(name).exists();
    let pyproject = std::fs::read_to_string(root.join("pyproject.toml")).unwrap_or_default();

    let (manager, test_command) = if exists("uv.lock") || pyproject.contains("[tool.uv]") {
        ("uv", "uv run pytest")
    } else if exists("poetry.lock") || pyproject.contains("[tool.poetry]") {
        ("poetry", "poetry run pytest")
    } else if exists("Pipfile") {
        ("pipenv", "pipenv run pytest")
    } else {
        ("pytest", "pytest")
    };

    ProjectEnv {
        language: format!("Python ({manager})"),
        test_command: test_command.to_string(),
        build_command: None,
    }
}

/// Pick a Node package manager (from the lock file) and a test command. The
/// project's `test` script is preferred because it reflects the maintainer's
/// intent; if there is none we fall back to a known runner, using `vitest run`
/// so the agent never gets stuck in watch mode.
fn detect_node_env(root: &Path) -> ProjectEnv {
    let exists = |name: &str| root.join(name).exists();

    let manager = if exists("pnpm-lock.yaml") {
        "pnpm"
    } else if exists("yarn.lock") {
        "yarn"
    } else if exists("bun.lockb") || exists("bun.lock") {
        "bun"
    } else {
        "npm"
    };

    let package_json: serde_json::Value = std::fs::read_to_string(root.join("package.json"))
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or(serde_json::Value::Null);

    let has_script = |name: &str| {
        package_json
            .get("scripts")
            .and_then(|scripts| scripts.get(name))
            .and_then(|value| value.as_str())
            .is_some()
    };
    let has_dependency = |name: &str| {
        ["dependencies", "devDependencies"].iter().any(|section| {
            package_json
                .get(section)
                .and_then(|deps| deps.get(name))
                .is_some()
        })
    };

    // `<pm> run <script>` is the unambiguous form across npm/pnpm/yarn/bun.
    // (Plain `bun test` would run Bun's built-in runner, and `npm build` is not
    // a valid shorthand, so we never rely on shorthands.)
    let run_script = |script: &str| format!("{manager} run {script}");

    let test_command = if has_script("test") {
        run_script("test")
    } else if has_dependency("vitest") {
        format!("{manager} exec vitest run")
    } else if has_dependency("jest") {
        format!("{manager} exec jest")
    } else {
        run_script("test")
    };

    let build_command = has_script("build").then(|| run_script("build"));

    ProjectEnv {
        language: format!("Node ({manager})"),
        test_command,
        build_command,
    }
}

fn validate_action(action: &PlannerAction, task: &task::TaskState) -> io::Result<()> {
    if task.budget.packet_tokens_used >= task.budget.hard_tokens {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hard token budget is exhausted",
        ));
    }

    match action.action {
        ActionKind::Search => require_text(action.args.query.as_deref(), "search.query"),
        ActionKind::ReadSymbol => require_text(action.args.symbol.as_deref(), "read_symbol.symbol"),
        ActionKind::ReadWindow => {
            require_text(action.args.file.as_deref(), "read_window.file")?;
            action.args.line.ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "read_window.line is required")
            })?;
            Ok(())
        }
        ActionKind::Trace => require_text(action.args.command.as_deref(), "trace.command"),
        ActionKind::ProposePatchPlan | ActionKind::Finish => Ok(()),
        ActionKind::AskUser => require_text(action.args.question.as_deref(), "ask_user.question"),
        ActionKind::PullContext => require_text(action.args.id.as_deref(), "pull.id"),
    }
}

fn execute_search(query: &str) -> io::Result<String> {
    let matches = search::search_exact(query, SEARCH_LIMIT)?;
    if matches.is_empty() {
        return Ok(format!("search `{query}` found no matches"));
    }

    let mut output = format!("search `{query}` found {} matches", matches.len());
    for item in matches.iter().take(5) {
        output.push_str(&format!(
            "\n- {}:{} {}",
            item.path,
            item.line_number,
            truncate(&item.line, 120)
        ));
    }
    Ok(output)
}

fn execute_read_symbol(task: &mut task::TaskState, symbol: &str) -> io::Result<String> {
    let root = patch::project_root()?;
    let target = if let Some((path, name)) = symbol.rsplit_once("::") {
        let (_, relative_path) = patch::resolve_existing_path(&root, path)?;
        format!("{}::{name}", relative_path.to_string_lossy())
    } else {
        symbol.to_string()
    };
    let item = read_symbol::read_symbol(&root, &target)?;
    record_inspected_digest(task, &root, Path::new(&item.path));
    Ok(format!(
        "read_symbol `{}` -> {} lines {}-{} ({} tokens)\n{}",
        symbol,
        item.path,
        item.symbol.start_line,
        item.symbol.end_line,
        format_count(item.estimated_tokens),
        truncate(&item.code, 1_500)
    ))
}

fn execute_read_window(
    task: &mut task::TaskState,
    file: &str,
    line: usize,
    radius: usize,
) -> io::Result<String> {
    let root = patch::project_root()?;
    let (path, relative_path) = patch::resolve_existing_path(&root, file)?;
    record_inspected_digest(task, &root, &relative_path);
    let window = read_window::read_window(path, line, radius, false)?;
    Ok(truncate(&window.render(), 2_000))
}

/// Record the current content digest of `path` (relative to the project
/// root) on `task.inspected_digests`, so a later patch attempt can tell
/// "dirty before we looked" apart from "changed after we looked" per the
/// working-tree ownership contract in `plan_3_safety_and_execution.md`.
/// Best-effort: a digest failure never fails the read itself.
fn record_inspected_digest(task: &mut task::TaskState, root: &Path, relative_path: &Path) {
    if let Ok(Some(digest)) = patch::digest_file(&root.join(relative_path)) {
        task.inspected_digests
            .insert(relative_path.to_string_lossy().to_string(), digest);
    }
}

fn require_text(value: Option<&str>, field: &str) -> io::Result<()> {
    if value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{field} is required"),
        ))
    }
}

fn format_optional(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn first_line(value: &str) -> &str {
    value.lines().next().unwrap_or(value)
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

fn trace_id() -> String {
    let suffix = Uuid::new_v4().simple().to_string();
    format!("agent-trace-{}", &suffix[..12])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_fixture() -> task::TaskState {
        task::TaskState {
            schema_version: 1,
            id: "task-1".to_string(),
            title: "Fix failing config test".to_string(),
            goal: "Fix failing config test".to_string(),
            acceptance: vec!["cargo test passes".to_string()],
            constraints: vec!["keep patch minimal".to_string()],
            budget: task::TaskBudget {
                soft_tokens: 40_000,
                hard_tokens: 80_000,
                packet_tokens_used: 362,
                raw_tokens_avoided: 0,
            },
            runs: Vec::new(),
            observations: vec![task::Observation {
                id: "obs1".to_string(),
                source: "run:failed".to_string(),
                kind: "test_failure".to_string(),
                summary: "assertion failed: error.to_string().contains(\"already existsabc\")"
                    .to_string(),
                locations: vec!["src/config.rs:213:9".to_string()],
                tokens: task::ObservationTokens {
                    raw: 100,
                    packet: 20,
                },
            }],
            hypotheses: vec![task::Hypothesis {
                id: "h1".to_string(),
                summary: "The assertion string may not match the implementation error message."
                    .to_string(),
                confidence: "high".to_string(),
                supporting_observations: vec!["obs1".to_string()],
                status: "open".to_string(),
            }],
            next_actions: Vec::new(),
            pending_agent_action: None,
            intent: None,
            current_failure: Some(task::CurrentFailure {
                kind: "test_failure".to_string(),
                summary: "config test failed".to_string(),
                locations: vec!["src/config.rs:213:9".to_string()],
                detail: None,
            }),
            closed_at: None,
            project: Some(task::ProjectCard {
                language: "Rust (cargo)".to_string(),
                test_command: "cargo test".to_string(),
                build_command: Some("cargo build".to_string()),
            }),
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
            workflow: workflow::Workflow::new(),
            pending_interaction: None,
            pending_approval: None,
            messages: Vec::new(),
            explicit_verify_commands: Vec::new(),
            inspected_digests: Default::default(),
            verification_results: Vec::new(),
        }
    }

    #[test]
    fn shadow_and_cutover_never_change_provider_visible_request() {
        let task = task_fixture();
        let tools = classifier_tools();
        let mut assembled = assemble_model_request(
            &task,
            1,
            NodeOp::ClassifyIntent,
            ModelPurpose::IntentClassification,
            None,
            "Classify task intent.\nTask: Fix failing config test",
            &tools,
            CLASSIFY_MAX_OUTPUT_TOKENS,
        )
        .unwrap();
        assembled.manifest.comparison_json = None;
        let legacy_request = assembled.request.clone();
        let primitive = primitive::primitive_for_node_op(&NodeOp::ClassifyIntent).unwrap();

        attach_context_comparison(
            &task,
            primitive,
            Path::new("."),
            &crate::config::ContextConfig {
                compiler_mode: crate::config::CompilerMode::Shadow,
                compiled_primitives: Vec::new(),
            },
            &mut assembled,
        )
        .unwrap();
        assert_eq!(assembled.request, legacy_request);
        let shadow: crate::context::comparison::ContextCompilationComparison =
            serde_json::from_str(assembled.manifest.comparison_json.as_deref().unwrap()).unwrap();
        assert!(!shadow.authoritative);

        assembled.manifest.comparison_json = None;
        attach_context_comparison(
            &task,
            primitive,
            Path::new("."),
            &crate::config::ContextConfig {
                compiler_mode: crate::config::CompilerMode::On,
                compiled_primitives: vec!["classify_intent".to_string()],
            },
            &mut assembled,
        )
        .unwrap();
        assert_eq!(assembled.request, legacy_request);
        let cutover: crate::context::comparison::ContextCompilationComparison =
            serde_json::from_str(assembled.manifest.comparison_json.as_deref().unwrap()).unwrap();
        assert!(cutover.authoritative);

        let on_config = crate::config::ContextConfig {
            compiler_mode: crate::config::CompilerMode::On,
            compiled_primitives: vec!["classify_intent".to_string(), "select_context".to_string()],
        };
        let compiled = compiled_context_for_config(&task, primitive, Path::new("."), &on_config)
            .unwrap()
            .unwrap();
        let compiled_goal = compiled_task_goal_value(compiled).unwrap();
        let compiled_request = assemble_model_request(
            &task,
            1,
            NodeOp::ClassifyIntent,
            ModelPurpose::IntentClassification,
            None,
            &format!("Classify task intent.\nTask: {compiled_goal}"),
            &tools,
            CLASSIFY_MAX_OUTPUT_TOKENS,
        )
        .unwrap();
        assert_eq!(compiled_request.request, legacy_request);

        let mut ranking_task = task.clone();
        let candidate_body = "fn load_config() -> bool { true }".to_string();
        let candidate = OffSiteCandidate {
            id: "c1".to_string(),
            symbol: "load_config".to_string(),
            path: "Cargo.toml".to_string(),
            start_line: 1,
            body: candidate_body.clone(),
        };
        ranking_task.available_context.push(task::AvailableContext {
            id: candidate.id.clone(),
            symbol: candidate.symbol.clone(),
            path: candidate.path.clone(),
            start_line: candidate.start_line,
            body: candidate.body.clone(),
            file_digest: Some(crate::context::artifact::file_content_digest(
                &std::fs::read("Cargo.toml").unwrap(),
            )),
            relevant: None,
        });
        let ranking_primitive = primitive::primitive_for_node_op(&NodeOp::SelectContext).unwrap();
        let compiled = compiled_context_for_config(
            &ranking_task,
            ranking_primitive,
            Path::new("."),
            &on_config,
        )
        .unwrap()
        .unwrap();
        let (compiled_failure, compiled_candidates) = compiled_ranking_values(compiled).unwrap();
        assert_eq!(
            serde_json::to_value(compiled_failure).unwrap(),
            serde_json::to_value(ranking_task.current_failure.as_ref().unwrap()).unwrap()
        );
        assert_eq!(compiled_candidates.len(), 1);
        assert_eq!(compiled_candidates[0].id, candidate.id);
        assert_eq!(compiled_candidates[0].symbol, candidate.symbol);
        assert_eq!(compiled_candidates[0].path, candidate.path);
        assert_eq!(compiled_candidates[0].start_line, candidate.start_line);
        assert_eq!(compiled_candidates[0].body, candidate.body);

        assembled.manifest.comparison_json = None;
        attach_context_comparison(
            &task,
            primitive,
            Path::new("."),
            &crate::config::ContextConfig::default(),
            &mut assembled,
        )
        .unwrap();
        assert_eq!(assembled.request, legacy_request);
        assert!(assembled.manifest.comparison_json.is_none());
    }

    #[test]
    fn patch_plan_prompt_dedupes_observation_matching_current_failure() {
        let mut task = task_fixture();
        task.current_failure.as_mut().unwrap().summary = task.observations[0].summary.clone();

        let prompt = patch_plan_prompt(&task);

        // The observation summary is identical to the current failure summary,
        // so it should not be repeated under "Known context".
        assert_eq!(prompt.matches("already existsabc").count(), 1);
    }

    #[test]
    fn patch_plan_prompt_includes_failure_detail_when_present() {
        let mut task = task_fixture();
        task.current_failure.as_mut().unwrap().detail =
            Some("assertion `left == right` failed\n  left: 1000\n right: 900".to_string());

        let prompt = patch_plan_prompt(&task);

        assert!(
            prompt.contains("left: 1000"),
            "expected the assertion diff to reach the patch-plan prompt: {prompt}"
        );
    }

    #[test]
    fn patch_plan_prompt_omits_detail_section_when_absent() {
        let task = task_fixture();
        assert!(task.current_failure.as_ref().unwrap().detail.is_none());

        let prompt = patch_plan_prompt(&task);

        assert!(!prompt.contains("Observed failure detail"));
    }

    #[test]
    fn compact_observation_strips_gutter_and_diagnostic_metadata() {
        let raw = "File: src/lib.rs\nLines: 1-2\nEstimated tokens: 12\n<code>\n    1 | fn foo() {}\n    2 | fn bar() {}\n</code>\n";

        let compacted = compact_observation(raw);

        assert!(!compacted.contains("Lines:"));
        assert!(!compacted.contains("Estimated tokens:"));
        assert!(!compacted.contains("1 | fn foo"));
        assert!(compacted.contains("fn foo() {}"));
        assert!(compacted.contains("fn bar() {}"));
    }

    #[test]
    fn planner_prompt_stays_small_and_action_oriented() {
        let prompt = planner_prompt(&task_fixture());

        assert!(prompt.contains("Goal: Fix failing config test"));
        assert!(prompt.contains("h1 high:"));
        // action menu lives in the tool schemas, not the prompt
        assert!(!prompt.contains("AVAILABLE ACTIONS"));
        assert!(!prompt.contains("read_symbol"));
    }

    #[test]
    fn planner_prompt_includes_detected_environment() {
        // HayCut itself is a cargo project, so the deterministic environment
        // card should tell the model how to run the tests without asking.
        let prompt = planner_prompt(&task_fixture());

        assert!(prompt.contains("ENVIRONMENT"));
        assert!(prompt.contains("Test: cargo test"));
    }

    #[test]
    fn planner_prompt_template_has_no_blank_lines_or_empty_sections() {
        // The Jinja block-trimming must not leave stray blank lines, and an
        // empty task must omit optional sections entirely (not print "none").
        let mut task = task_fixture();
        task.acceptance.clear();
        task.constraints.clear();
        task.observations.clear();
        task.hypotheses.clear();
        task.current_failure = None;

        let prompt = planner_prompt(&task);

        assert!(
            !prompt.contains("\n\n"),
            "prompt had a blank line:\n{prompt}"
        );
        assert!(!prompt.contains("none"));
        assert!(!prompt.contains("Acceptance:"));
        assert!(!prompt.contains("CURRENT FAILURE"));
        assert!(!prompt.contains("KNOWN CONTEXT"));
        // Required sections remain, in order.
        let task_at = prompt.find("TASK\n").unwrap();
        let env_at = prompt.find("ENVIRONMENT\n").unwrap();
        let budget_at = prompt.find("BUDGET\n").unwrap();
        assert!(task_at < env_at && env_at < budget_at);
    }

    #[test]
    fn detect_project_env_recognises_marker_files() {
        let dir = std::env::temp_dir().join(format!("haycut-env-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();

        assert!(detect_project_env(&dir).is_none());

        std::fs::write(dir.join("package.json"), "{}").unwrap();
        let env = detect_project_env(&dir).expect("package.json should be detected");
        assert_eq!(env.language, "Node (npm)");
        assert_eq!(env.test_command, "npm run test");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detect_node_env_prefers_test_script_and_lockfile_manager() {
        let dir = std::env::temp_dir().join(format!("haycut-node-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{ "scripts": { "test": "vitest run", "build": "tsc" } }"#,
        )
        .unwrap();

        let env = detect_node_env(&dir);
        assert_eq!(env.language, "Node (pnpm)");
        assert_eq!(env.test_command, "pnpm run test");
        assert_eq!(env.build_command.as_deref(), Some("pnpm run build"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detect_node_env_falls_back_to_vitest_run_without_watch() {
        let dir = std::env::temp_dir().join(format!("haycut-node-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{ "devDependencies": { "vitest": "^1.0.0" } }"#,
        )
        .unwrap();

        let env = detect_node_env(&dir);
        assert_eq!(env.test_command, "npm exec vitest run");
        assert!(env.build_command.is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detect_python_env_uses_dependency_manager() {
        let dir = std::env::temp_dir().join(format!("haycut-py-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pyproject.toml"), "[tool.uv]\n").unwrap();

        let uv_env = detect_python_env(&dir);
        assert_eq!(uv_env.language, "Python (uv)");
        assert_eq!(uv_env.test_command, "uv run pytest");

        std::fs::write(dir.join("poetry.lock"), "").unwrap();
        // uv still wins because its marker is checked first.
        assert_eq!(detect_python_env(&dir).test_command, "uv run pytest");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_intent_maps_labels_and_defaults_safely() {
        assert_eq!(
            parse_intent(Some("debug_failure")),
            task::TaskIntent::DebugFailure
        );
        assert_eq!(
            parse_intent(Some("implement_feature")),
            task::TaskIntent::ImplementFeature
        );
        assert_eq!(parse_intent(Some("refactor")), task::TaskIntent::Refactor);
        // Unknown or missing labels fall back to the non-destructive intent.
        assert_eq!(
            parse_intent(Some("nonsense")),
            task::TaskIntent::AnswerQuestion
        );
        assert_eq!(parse_intent(None), task::TaskIntent::AnswerQuestion);
    }

    #[test]
    fn terminal_exit_codes_distinguish_verified_from_incomplete_work() {
        assert_eq!(stop_exit_code(StopReason::Verified), 0);
        for reason in [
            StopReason::Blocked,
            StopReason::Failed,
            StopReason::LoopDetected,
            StopReason::BudgetExhausted,
            StopReason::MaxSteps,
        ] {
            assert_ne!(stop_exit_code(reason), 0, "{reason:?}");
        }
    }

    #[test]
    fn parse_location_handles_line_and_column_forms() {
        assert_eq!(
            parse_location("src/cart.rs:12"),
            Some(("src/cart.rs".to_string(), 12))
        );
        assert_eq!(
            parse_location("src/cart.rs:12:5"),
            Some(("src/cart.rs".to_string(), 12))
        );
        assert_eq!(parse_location("src/cart.rs"), None);
        assert_eq!(parse_location(":12"), None);
    }

    #[test]
    fn extract_relevant_ids_normalizes_labeled_entries() {
        let known: std::collections::HashSet<String> = ["c1".to_string()].into_iter().collect();
        let args = serde_json::json!({
            "candidates": ["c1: apply_bulk_discount @ src/pricing.rs"]
        });

        let ids = extract_relevant_ids(&args, &known);

        assert_eq!(ids, known);
    }

    #[test]
    fn extract_relevant_ids_drops_unknown_ids() {
        let known: std::collections::HashSet<String> = ["c1".to_string()].into_iter().collect();
        let args = serde_json::json!({ "relevant_ids": ["c9", "bogus"] });

        let ids = extract_relevant_ids(&args, &known);

        assert!(ids.is_empty());
    }

    #[test]
    fn action_from_tool_call_maps_sym_to_read_symbol() {
        let args = serde_json::json!({ "symbol": "create_default_config_at", "reason": "inspect" });
        let action = action_from_tool_call("sym", args).expect("should map");

        assert_eq!(action.action, ActionKind::ReadSymbol);
        assert_eq!(
            action.args.symbol.as_deref(),
            Some("create_default_config_at")
        );
        assert_eq!(action.reason, "inspect");
    }

    #[test]
    fn action_from_tool_call_rejects_unknown_tool() {
        let error = action_from_tool_call("bogus", serde_json::json!({})).unwrap_err();
        assert!(error.to_string().contains("unknown tool"));
    }

    #[test]
    fn agent_action_preserves_paths_with_spaces_and_quoted_command_args() {
        // The old bridge stringified the action into a `haycut ...` command
        // and re-split it on whitespace, corrupting paths with spaces and
        // arguments containing quotes. The typed AgentAction must round-trip
        // these verbatim.
        let read_window = PlannerAction {
            action: ActionKind::ReadWindow,
            args: ActionArgs {
                file: Some("src/my dir/has spaces.rs".to_string()),
                line: Some(10),
                radius: Some(5),
                ..Default::default()
            },
            reason: "inspect".to_string(),
        };
        assert_eq!(
            agent_action_from_planner_action(&read_window),
            AgentAction::ReadWindow {
                path: PathBuf::from("src/my dir/has spaces.rs"),
                line: 10,
                radius: 5,
            }
        );

        let trace = PlannerAction {
            action: ActionKind::Trace,
            args: ActionArgs {
                command: Some("echo".to_string()),
                args: vec!["say \"hello world\"".to_string(), "a b".to_string()],
                ..Default::default()
            },
            reason: "run".to_string(),
        };
        assert_eq!(
            agent_action_from_planner_action(&trace),
            AgentAction::RunCommand {
                program: "echo".to_string(),
                args: vec!["say \"hello world\"".to_string(), "a b".to_string()],
            }
        );
    }

    #[test]
    fn rejects_missing_read_symbol_argument() {
        let action = PlannerAction {
            action: ActionKind::ReadSymbol,
            args: ActionArgs::default(),
            reason: "missing arg".to_string(),
        };

        let error = validate_action(&action, &task_fixture()).expect_err("action should fail");

        assert!(error.to_string().contains("read_symbol.symbol"));
    }

    fn available_context_fixture() -> task::AvailableContext {
        task::AvailableContext {
            id: "c1".to_string(),
            symbol: "apply_bulk_discount".to_string(),
            path: "src/pricing.rs".to_string(),
            start_line: 2,
            body: "pub fn apply_bulk_discount() {}".to_string(),
            file_digest: None,
            relevant: None,
        }
    }

    #[test]
    fn pull_context_injects_body_and_clears_candidate() {
        let mut task = task_fixture();
        task.available_context.push(available_context_fixture());

        let result = execute_pull_context(&mut task, "c1").unwrap();

        assert!(result.summary.contains("apply_bulk_discount"));
        assert!(task.available_context.is_empty());
        let observation = task
            .observations
            .iter()
            .find(|observation| observation.source == PULLED_CONTEXT_SOURCE)
            .expect("pulled observation must be recorded");
        assert!(observation.summary.contains("pub fn apply_bulk_discount"));
        assert_eq!(observation.locations, vec!["src/pricing.rs:2".to_string()]);
    }

    #[test]
    fn pull_context_unknown_id_leaves_state_unchanged() {
        let mut task = task_fixture();

        let result = execute_pull_context(&mut task, "missing").unwrap();

        assert!(result.summary.contains("no available context"));
        assert!(
            task.observations
                .iter()
                .all(|o| o.source != PULLED_CONTEXT_SOURCE)
        );
    }

    #[test]
    fn planner_tools_only_offer_pull_when_context_available() {
        let mut task = task_fixture();
        assert!(!planner_tools(&task).iter().any(|tool| tool.name == "pull"));

        task.available_context.push(available_context_fixture());
        assert!(planner_tools(&task).iter().any(|tool| tool.name == "pull"));
    }

    #[test]
    fn patch_scope_violation_fires_only_for_relevant_unpulled_candidates() {
        let mut task = task_fixture();
        task.patch_edits = Some(vec![task::PatchEdit::Replace {
            path: "src/cart.rs".to_string(),
            find: "x".to_string(),
            replace: "y".to_string(),
            expected_digest: None,
        }]);

        // Unknown relevance never blocks the patch.
        let mut candidate = available_context_fixture();
        task.available_context.push(candidate.clone());
        assert!(patch_scope_violation(&task).is_none());

        // Marked relevant and left unpulled, and the patch never touches its
        // file: the guard must fire.
        candidate.relevant = Some(true);
        task.available_context = vec![candidate];
        let reason = patch_scope_violation(&task).expect("guard should fire");
        assert!(reason.contains("src/pricing.rs"));

        // Once a guard observation exists, it steps aside (fires at most once).
        task.observations.push(task::Observation {
            id: "obs-guard".to_string(),
            source: PATCH_GUARD_SOURCE.to_string(),
            kind: "patch_rejected".to_string(),
            summary: reason,
            locations: Vec::new(),
            tokens: task::ObservationTokens { raw: 0, packet: 0 },
        });
        assert!(patch_scope_violation(&task).is_none());
    }

    #[test]
    fn collect_graph_candidates_follows_call_stack_across_files() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("evals/cases/split_context_off_by_one_rs/repo");
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&repo).unwrap();

        let failure = task::CurrentFailure {
            kind: "test_failure".to_string(),
            summary: "ten_units_qualifies_for_bulk_discount failed".to_string(),
            locations: vec!["src/cart.rs:4".to_string()],
            detail: None,
        };
        let candidates = collect_graph_candidates(&failure);

        std::env::set_current_dir(original_dir).unwrap();

        assert!(
            candidates
                .iter()
                .any(|c| c.symbol == "apply_bulk_discount" && c.path.ends_with("pricing.rs")),
            "expected apply_bulk_discount@pricing.rs among {:?}",
            candidates
                .iter()
                .map(|c| (&c.symbol, &c.path))
                .collect::<Vec<_>>()
        );
    }

    fn context_candidate(id: &str, relevant: Option<bool>, body: &str) -> task::AvailableContext {
        task::AvailableContext {
            id: id.to_string(),
            symbol: format!("symbol_{id}"),
            path: "src/lib.rs".to_string(),
            start_line: 1,
            body: body.to_string(),
            file_digest: None,
            relevant,
        }
    }

    #[test]
    fn eager_load_pulls_small_confident_set_and_queues_plan_patch() {
        let mut task = task_fixture();
        task.available_context = vec![
            context_candidate("c1", Some(true), "fn apply_bulk_discount() {}"),
            context_candidate("c2", Some(false), "fn unrelated() {}"),
        ];

        let summary = maybe_eager_load_context(&mut task);

        assert!(summary.is_some());
        assert_eq!(task.pending_agent_action, Some(AgentAction::PlanPatch));
        // The relevant candidate was pulled (removed from available_context)
        // and turned into an observation; the irrelevant one is left alone.
        assert!(!task.available_context.iter().any(|c| c.id == "c1"));
        assert!(task.available_context.iter().any(|c| c.id == "c2"));
        assert!(
            task.observations
                .iter()
                .any(|obs| obs.source == PULLED_CONTEXT_SOURCE)
        );
    }

    #[test]
    fn eager_load_skips_when_no_relevant_candidates() {
        let mut task = task_fixture();
        task.available_context = vec![
            context_candidate("c1", None, "fn maybe() {}"),
            context_candidate("c2", Some(false), "fn unrelated() {}"),
        ];

        let summary = maybe_eager_load_context(&mut task);

        assert!(summary.is_none());
        assert_eq!(task.pending_agent_action, None);
        assert_eq!(task.available_context.len(), 2);
    }

    #[test]
    fn eager_load_skips_when_relevant_set_too_large() {
        let mut task = task_fixture();
        task.available_context = (1..=EAGER_CONTEXT_MAX + 1)
            .map(|n| context_candidate(&format!("c{n}"), Some(true), "fn f() {}"))
            .collect();

        let summary = maybe_eager_load_context(&mut task);

        assert!(summary.is_none());
        assert_eq!(task.pending_agent_action, None);
        assert_eq!(
            task.available_context.len(),
            EAGER_CONTEXT_MAX + 1,
            "nothing should be pulled when the relevant set exceeds the threshold"
        );
    }
}
