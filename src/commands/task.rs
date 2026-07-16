use std::{fs, io, path::Path};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    cli::TaskCommand,
    commands::{read_window::DEFAULT_RADIUS, trace::CommandTrace},
    compactor::CompactPacket,
    config::Config,
    evidence::{EvidencePacket, PrimaryDiagnostic},
    extract::DiagnosticKind,
    store::{self, RUN_STORE_PATH, StoredTask},
    util::format_count,
};

pub fn run(command: TaskCommand) -> i32 {
    match command {
        TaskCommand::Start { title, verify } => run_start(title, verify),
        TaskCommand::Status => run_status(),
        TaskCommand::List => run_list(),
        TaskCommand::Close => run_close(),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TaskState {
    pub schema_version: u8,
    pub id: String,
    pub title: String,
    pub goal: String,
    pub acceptance: Vec<String>,
    pub constraints: Vec<String>,
    pub budget: TaskBudget,
    pub runs: Vec<TaskRun>,
    pub observations: Vec<Observation>,
    pub hypotheses: Vec<Hypothesis>,
    pub next_actions: Vec<NextAction>,
    /// Typed action queued by the strong planner for `ReadContext` to execute
    /// deterministically, or a terminal decision (`PlanPatch`/`AskUser`/
    /// `Finish`) awaiting routing. Distinct from `next_actions`, which is the
    /// heuristic evidence-derived suggestion queue surfaced by `task status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_agent_action: Option<crate::commands::agent::AgentAction>,
    /// Cheap-classifier verdict of what kind of work this task is. Set once on
    /// the first agent step and used to route deterministic shortcuts (e.g.
    /// reproduce-first for debug tasks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<TaskIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_failure: Option<CurrentFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,

    // Agent state-machine additions.
    /// Detected project environment (language, build/test commands).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectCard>,
    /// Resolved verification plan derived from the project card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationPlan>,
    /// Flight-recorder route of steps executed so far.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route: Vec<RouteEntry>,
    /// Rendered patch summary produced by the strong-model planner. Kept for
    /// human/eval-facing display; when `patch_edits` is set this is derived
    /// from it, otherwise it may hold free-form planner text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_text: Option<String>,
    /// Structured, machine-appliable edits produced by the strong-model
    /// planner via the `propose_edits` tool. Each edit names an exact
    /// find/replace anchor so a deterministic applier can apply it without
    /// re-parsing prose or diff hunks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_edits: Option<Vec<PatchEdit>>,
    /// Whether the planned patch has been (stub) applied.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub patch_applied: bool,
    /// Whether the current plan was shown without mutation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub patch_previewed: bool,
    /// Mutation must be explicitly authorized by `agent run --apply`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub apply_requested: bool,
    /// Final agent outcome, recorded when the workflow reaches a terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<crate::commands::agent::StopReason>,
    /// Number of retry-fix loops performed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retry_count: usize,
    /// Signature of the failure that triggered the last retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_signature: Option<String>,
    /// Off-site symbols found by deterministic call-stack follow, listed to
    /// the strong planner but not injected into any prompt until it `pull`s
    /// one by id. Keeps default context minimal while making the body one
    /// tool call away.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_context: Vec<AvailableContext>,
    /// Versioned, serializable description of the compatibility workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_spec: Option<crate::commands::agent::workflow_spec::WorkflowSpec>,
    /// Self-writing DAG of nodes driving the agent state machine.
    #[serde(default)]
    pub workflow: crate::commands::agent::workflow::Workflow,

    // Interactive engine session state (see `agent::engine`).
    /// Question the workflow is blocked on, awaiting a user reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_interaction: Option<crate::commands::agent::engine::PendingInteraction>,
    /// Proposed mutation awaiting explicit user approval or rejection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_approval: Option<crate::commands::agent::engine::ApprovalRequest>,
    /// Durable transcript of user/agent messages exchanged in a session
    /// (questions, replies, rejections, steering instructions).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<crate::commands::agent::engine::TaskMessage>,
    /// Raw user-supplied verification commands from `task start --verify`
    /// (comma-separated) or the REPL's `verify <command>`, kept so
    /// `execute_resolve_verification` can augment or override
    /// project-detected defaults instead of only falling back to them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub explicit_verify_commands: Vec<String>,
    /// Content digest recorded for each file the agent has read as context,
    /// keyed by path relative to the project root. Used for optimistic
    /// concurrency: a patch may apply to a pre-existing dirty file if its
    /// current content still matches the digest recorded here, and is
    /// refused as a recoverable conflict if it doesn't.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub inspected_digests:
        std::collections::HashMap<String, crate::commands::agent::patch::FileDigest>,
    /// Structured, per-check outcomes from the most recent
    /// `RunFinalVerification` step: one entry per `VerificationCheck` in
    /// `verification.checks`, in order, so callers (eval harness, dashboard)
    /// can distinguish a failing required check from a failing optional one
    /// without re-parsing the free-text summary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_results: Vec<VerificationCheckResult>,
}

/// A candidate off-site symbol surfaced by deterministic call-stack follow.
/// `relevant` is `Some(bool)` once the weak model has judged it, `None` if
/// the gate was unavailable or failed (never dropped either way).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AvailableContext {
    pub id: String,
    pub symbol: String,
    pub path: String,
    pub start_line: usize,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_digest: Option<String>,
    #[serde(default)]
    pub relevant: Option<bool>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Detected project environment used by the agent state machine.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProjectCard {
    pub language: String,
    pub test_command: String,
    pub build_command: Option<String>,
}

/// A verification command to run, independent of any particular shell.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VerificationCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl VerificationCommand {
    /// Parse a shell-word-split command string, e.g. `"cargo test"`.
    pub fn parse(input: &str) -> Option<Self> {
        let mut parts = input.split_whitespace();
        let program = parts.next()?.to_string();
        Some(VerificationCommand {
            program,
            args: parts.map(str::to_string).collect(),
        })
    }

    pub fn as_vec(&self) -> Vec<String> {
        std::iter::once(self.program.clone())
            .chain(self.args.iter().cloned())
            .collect()
    }

    pub fn display(&self) -> String {
        self.as_vec().join(" ")
    }
}

/// What part of the project a `VerificationCheck` is expected to exercise.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationScope {
    #[default]
    FullProject,
    ChangedFilesOnly,
    Targeted,
}

/// One verification check: a command to run, whether it must pass for the
/// task to be considered done, and what part of the project it exercises.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VerificationCheck {
    pub command: VerificationCommand,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub scope: VerificationScope,
}

fn default_true() -> bool {
    true
}

/// Structured, user-overridable verification plan: an ordered list of
/// checks. Project detection seeds sensible defaults (`ResolveVerification`);
/// explicit `--verify`/`verify <command>` input augments or overrides them
/// rather than only being a fallback.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VerificationPlan {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<VerificationCheck>,
    /// Exit code the *first required* check is expected to produce before
    /// any fix is applied (e.g. 101 for a failing `cargo test` baseline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_baseline_exit: Option<i32>,
}

impl VerificationPlan {
    /// The command baseline/legacy call sites should run: the first
    /// required check if any, else the first check at all.
    pub fn primary_command(&self) -> Option<&VerificationCommand> {
        self.checks
            .iter()
            .find(|check| check.required)
            .or_else(|| self.checks.first())
            .map(|check| &check.command)
    }
}

/// The outcome of running one `VerificationCheck` during
/// `RunFinalVerification`: what ran, whether it was required, and whether it
/// passed. Persisted on `TaskState` so the eval harness and dashboard can
/// report structured pass/fail per check instead of re-parsing free text.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VerificationCheckResult {
    pub command: VerificationCommand,
    pub required: bool,
    pub scope: VerificationScope,
    pub exit_code: i32,
    pub passed: bool,
}

/// One structured, machine-appliable file operation. `Replace` keeps exact,
/// single-occurrence anchor matching (cheap and safe); `Create`/`Delete`/
/// `Rename` extend the vocabulary beyond in-place edits. `Delete`/`Rename`
/// carry an `expected_digest` so they only apply against the file contents
/// the agent actually inspected; `Replace` may optionally carry one too.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PatchEdit {
    Replace {
        path: String,
        find: String,
        replace: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_digest: Option<crate::commands::agent::patch::FileDigest>,
    },
    Create {
        path: String,
        content: String,
        /// Overwrite an existing file instead of failing. Must be explicitly
        /// set; `Create` refuses to clobber an existing file by default.
        #[serde(default)]
        overwrite: bool,
    },
    Delete {
        path: String,
        expected_digest: crate::commands::agent::patch::FileDigest,
    },
    Rename {
        from: String,
        to: String,
        expected_digest: crate::commands::agent::patch::FileDigest,
    },
}

impl PatchEdit {
    /// The file this edit primarily targets, for scope/display purposes.
    /// `Rename` reports its source path, since that's what a failure's call
    /// path would already reference.
    pub fn primary_path(&self) -> &str {
        match self {
            PatchEdit::Replace { path, .. } => path,
            PatchEdit::Create { path, .. } => path,
            PatchEdit::Delete { path, .. } => path,
            PatchEdit::Rename { from, .. } => from,
        }
    }
}

/// One entry in the agent route flight recorder.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RouteEntry {
    pub step: String,
    pub executor: crate::commands::agent::ExecutorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primitive_id: Option<crate::commands::agent::primitive::PrimitiveId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primitive_version: Option<crate::commands::agent::primitive::PrimitiveVersion>,
    pub outcome: String,
}

/// Coarse task classification produced by the weak classification model.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskIntent {
    /// Something is broken or failing; reproduce it before reasoning.
    DebugFailure,
    /// Add or change functionality.
    ImplementFeature,
    /// Restructure code without changing behaviour.
    Refactor,
    /// Look something up or explain; no code change implied.
    AnswerQuestion,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TaskBudget {
    pub soft_tokens: usize,
    pub hard_tokens: usize,
    #[serde(default)]
    pub packet_tokens_used: usize,
    #[serde(default)]
    pub raw_tokens_avoided: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TaskRun {
    pub id: String,
    pub command: String,
    pub exit_code: i32,
    pub raw_tokens: usize,
    pub packet_tokens: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Observation {
    pub id: String,
    pub source: String,
    pub kind: String,
    pub summary: String,
    pub locations: Vec<String>,
    pub tokens: ObservationTokens,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ObservationTokens {
    pub raw: usize,
    pub packet: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Hypothesis {
    pub id: String,
    pub summary: String,
    pub confidence: String,
    pub supporting_observations: Vec<String>,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NextAction {
    pub command: String,
    pub reason: String,
    pub expected_answer: String,
    pub estimated_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypothesis: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CurrentFailure {
    pub kind: String,
    pub summary: String,
    pub locations: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub fn attach_current_run(
    trace: &CommandTrace,
    packet: &CompactPacket,
    evidence: &EvidencePacket,
) -> io::Result<()> {
    let mut task = load_current()?;
    let run_id = evidence.run_id.clone();

    // `packet_tokens_used` also accumulates model-call costs (see
    // `execute_classify_intent`/`execute_plan_patch` in agent.rs). Only add
    // this run's cost once, on first attach — recomputing the total from
    // `task.runs` here would silently discard those model-token additions
    // every time a command runs after a model call.
    if !task.runs.iter().any(|run| run.id == run_id) {
        task.runs.push(TaskRun {
            id: run_id.clone(),
            command: packet.command.clone(),
            exit_code: trace.exit_code,
            raw_tokens: packet.raw_tokens,
            packet_tokens: packet.packet_tokens,
        });
        task.budget.packet_tokens_used = task
            .budget
            .packet_tokens_used
            .saturating_add(packet.packet_tokens);
    }

    task.budget.raw_tokens_avoided = task
        .runs
        .iter()
        .map(|run| run.raw_tokens.saturating_sub(run.packet_tokens))
        .sum();

    if let Some(observation) = observation_from_evidence(&task, evidence) {
        task.current_failure = Some(CurrentFailure {
            kind: observation.kind.clone(),
            summary: observation.summary.clone(),
            locations: observation.locations.clone(),
            detail: evidence
                .primary_diagnostic
                .as_ref()
                .map(|primary| primary.message.clone()),
        });
        let observation_id = observation.id.clone();
        let new_hypotheses = hypotheses_for_observation(evidence, &observation);
        task.observations.push(observation);

        for mut hypothesis in new_hypotheses {
            if task
                .hypotheses
                .iter()
                .any(|existing| existing.summary == hypothesis.summary)
            {
                continue;
            }
            hypothesis.id = format!("h{}", task.hypotheses.len() + 1);
            task.hypotheses.push(hypothesis);
        }

        task.next_actions = next_actions_for(
            evidence,
            task.hypotheses.last(),
            &observation_id,
            &trace.working_directory,
        );
    }

    save_current(&task)
}

pub fn load_current_next_actions() -> io::Result<Option<Vec<NextAction>>> {
    match load_current() {
        Ok(task) => Ok(Some(task.next_actions)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn run_start(title: String, verify: Option<String>) -> i32 {
    match start_current(title, verify) {
        Ok(task) => {
            println!("Started task {}", task.id);
            0
        }
        Err(error) => {
            eprintln!("Error saving task: {error}");
            1
        }
    }
}

pub fn start_current(title: String, verify: Option<String>) -> io::Result<TaskState> {
    let config =
        Config::load_from_current_dir().map_err(|error| io::Error::other(error.to_string()))?;

    let mut task = TaskState {
        schema_version: 1,
        id: task_id(),
        title: title.clone(),
        goal: title,
        acceptance: verify
            .as_deref()
            .map(|command| vec![format!("{command} passes")])
            .unwrap_or_default(),
        constraints: Vec::new(),
        budget: TaskBudget {
            soft_tokens: config.token.soft_budget as usize,
            hard_tokens: config.token.hard_budget as usize,
            packet_tokens_used: 0,
            raw_tokens_avoided: 0,
        },
        runs: Vec::new(),
        observations: Vec::new(),
        hypotheses: Vec::new(),
        next_actions: Vec::new(),
        pending_agent_action: None,
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
        pending_interaction: None,
        pending_approval: None,
        messages: Vec::new(),
        explicit_verify_commands: verify
            .as_deref()
            .map(|commands| {
                commands
                    .split(',')
                    .map(str::trim)
                    .filter(|command| !command.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        inspected_digests: Default::default(),
        verification_results: Vec::new(),
    };
    task.workflow_spec =
        Some(crate::commands::agent::workflow_spec::compile_compatibility_spec(&task));

    save_current(&task)?;
    Ok(task)
}

fn run_status() -> i32 {
    match load_current() {
        Ok(task) => {
            print_status(&task);
            0
        }
        Err(error) => {
            eprintln!("Error loading task: {error}");
            1
        }
    }
}

fn run_list() -> i32 {
    let tasks = match store::list_tasks(Path::new(RUN_STORE_PATH)) {
        Ok(tasks) => tasks,
        Err(error) => {
            eprintln!("Error loading tasks: {error}");
            return 1;
        }
    };

    if tasks.is_empty() {
        println!("No HayCut tasks found.");
        return 0;
    }

    for task in tasks {
        println!("{}  {}  {}", task.status, task.id, task.title);
    }
    0
}

fn run_close() -> i32 {
    match close_current() {
        Ok(task) => {
            println!("Closed task {}", task.id);
            0
        }
        Err(error) => {
            eprintln!("Error closing task: {error}");
            1
        }
    }
}

fn close_current() -> io::Result<TaskState> {
    let mut task = load_current()?;
    task.closed_at = Some(Utc::now());
    let contents = serde_json::to_string_pretty(&task).map_err(io::Error::other)?;
    let closed_at = task
        .closed_at
        .as_ref()
        .expect("closed_at should be set")
        .to_rfc3339();
    store::close_current_task(Path::new(RUN_STORE_PATH), &contents, &closed_at)?;
    Ok(task)
}

pub fn load_current() -> io::Result<TaskState> {
    let stored = store::current_task(Path::new(RUN_STORE_PATH))?;
    serde_json::from_str(&stored.task_json).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid task state {}: {error}", stored.id),
        )
    })
}

pub fn save_current(task: &TaskState) -> io::Result<()> {
    let contents = serde_json::to_string_pretty(task).map_err(io::Error::other)?;
    store::upsert_task(
        Path::new(RUN_STORE_PATH),
        &StoredTask {
            id: task.id.clone(),
            title: task.title.clone(),
            status: if task.closed_at.is_some() {
                "closed".to_string()
            } else {
                "open".to_string()
            },
            task_json: contents,
            updated_at: Utc::now().to_rfc3339(),
        },
        task.closed_at.is_none(),
    )
}

fn observation_from_evidence(task: &TaskState, evidence: &EvidencePacket) -> Option<Observation> {
    let failure = evidence.likely_failure.as_ref()?;
    let locations = observation_locations(evidence.primary_diagnostic.as_ref());

    Some(Observation {
        id: format!("obs{}", task.observations.len() + 1),
        source: format!("run:{}", evidence.run_id),
        kind: failure.kind.clone(),
        summary: failure.summary.clone(),
        locations,
        tokens: ObservationTokens {
            raw: evidence.token_summary.raw_tokens,
            packet: evidence.token_summary.packet_tokens,
        },
    })
}

fn observation_locations(primary: Option<&PrimaryDiagnostic>) -> Vec<String> {
    let Some(primary) = primary else {
        return Vec::new();
    };

    match (primary.file.as_deref(), primary.line, primary.column) {
        (Some(file), Some(line), Some(column)) => vec![format!("{file}:{line}:{column}")],
        (Some(file), Some(line), None) => vec![format!("{file}:{line}")],
        (Some(file), None, _) => vec![file.to_string()],
        _ => Vec::new(),
    }
}

fn hypotheses_for_observation(
    evidence: &EvidencePacket,
    observation: &Observation,
) -> Vec<Hypothesis> {
    let mut hypotheses = Vec::new();

    if let Some(literal) = suspicious_assertion_literal(evidence) {
        hypotheses.push(Hypothesis {
            id: String::new(),
            summary: format!(
                "The assertion string `{literal}` may not match the implementation error message."
            ),
            confidence: "high".to_string(),
            supporting_observations: vec![observation.id.clone()],
            status: "open".to_string(),
        });
    }

    if evidence.diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == DiagnosticKind::RustCompileError
            && diagnostic.message.contains("missing")
            && diagnostic.message.contains("field")
    }) {
        hypotheses.push(Hypothesis {
            id: String::new(),
            summary: "A struct initializer may be missing a newly required field.".to_string(),
            confidence: "high".to_string(),
            supporting_observations: vec![observation.id.clone()],
            status: "open".to_string(),
        });
    }

    if hypotheses.is_empty()
        && evidence
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::Panic)
    {
        hypotheses.push(Hypothesis {
            id: String::new(),
            summary: "The panic location should be inspected with the function under test."
                .to_string(),
            confidence: "medium".to_string(),
            supporting_observations: vec![observation.id.clone()],
            status: "open".to_string(),
        });
    }

    hypotheses
}

fn suspicious_assertion_literal(evidence: &EvidencePacket) -> Option<String> {
    evidence
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.kind == DiagnosticKind::Panic)
        .find_map(|diagnostic| literal_inside(&diagnostic.message, ".contains(\""))
}

fn literal_inside(message: &str, prefix: &str) -> Option<String> {
    let start = message.find(prefix)? + prefix.len();
    let rest = &message[start..];
    let end = rest.find('\"')?;
    let literal = &rest[..end];

    (!literal.is_empty()).then(|| literal.to_string())
}

fn next_actions_for(
    evidence: &EvidencePacket,
    hypothesis: Option<&Hypothesis>,
    observation_id: &str,
    cwd: &Path,
) -> Vec<NextAction> {
    if hypothesis
        .map(|h| h.summary.contains("assertion string"))
        .unwrap_or(false)
        && let Some(symbol) = function_under_test(evidence, cwd)
    {
        let hypothesis_id = hypothesis.map(|h| h.id.clone());
        return vec![NextAction {
            command: format!("haycut read-symbol {symbol}"),
            reason: hypothesis
                .map(|h| {
                    format!(
                        "Hypothesis {} depends on the actual error produced by {symbol}.",
                        h.id
                    )
                })
                .unwrap_or_else(|| format!("Observation {observation_id} points to {symbol}.")),
            expected_answer:
                "Whether the implementation error message contains the asserted literal."
                    .to_string(),
            estimated_tokens: 500,
            hypothesis: hypothesis_id,
        }];
    }

    let target = evidence
        .primary_diagnostic
        .as_ref()
        .and_then(|primary| {
            primary
                .file
                .as_ref()
                .zip(primary.line)
                .map(|(file, line)| format!("{file}:{line}"))
        })
        .or_else(|| {
            evidence
                .context_items
                .first()
                .map(|item| item.target.clone())
        });

    let Some(target) = target else {
        return Vec::new();
    };

    let command = if let Some((file, line)) = target.rsplit_once(':') {
        if line.parse::<usize>().is_ok() {
            format!("haycut read-window {file} --line {line} --radius {DEFAULT_RADIUS}")
        } else {
            format!("haycut read-symbol {target}")
        }
    } else {
        format!("haycut read-symbol {target}")
    };

    let hypothesis_id = hypothesis.map(|h| h.id.clone());
    let reason = hypothesis
        .map(|h| format!("Hypothesis {} depends on the code at {target}.", h.id))
        .unwrap_or_else(|| format!("Observation {observation_id} points to {target}."));

    vec![NextAction {
        command,
        reason,
        expected_answer: expected_answer_for(hypothesis, &target),
        estimated_tokens: 500,
        hypothesis: hypothesis_id,
    }]
}

pub(crate) fn function_under_test(evidence: &EvidencePacket, cwd: &Path) -> Option<String> {
    let primary = evidence.primary_diagnostic.as_ref()?;
    let file = primary.file.as_ref()?;
    let line = primary.line?;
    let path = if Path::new(file).is_absolute() {
        Path::new(file).to_path_buf()
    } else {
        cwd.join(file)
    };
    let contents = fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = contents.lines().collect();
    let start = line.saturating_sub(8).max(1);
    let end = line.min(lines.len());

    lines[start - 1..end]
        .iter()
        .rev()
        .find_map(|line| function_call_name(line))
}

pub(crate) fn function_call_name(line: &str) -> Option<String> {
    let before_paren = line.split('(').next()?.trim_end();
    let name = before_paren
        .chars()
        .rev()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();

    if name.is_empty() || matches!(name.as_str(), "assert" | "contains" | "to_string") {
        None
    } else {
        Some(name)
    }
}

fn expected_answer_for(hypothesis: Option<&Hypothesis>, target: &str) -> String {
    if hypothesis
        .map(|h| h.summary.contains("assertion string"))
        .unwrap_or(false)
    {
        return "Whether the implementation error message contains the asserted literal."
            .to_string();
    }

    format!("Whether {target} explains the current failure.")
}

fn print_status(task: &TaskState) {
    println!("HayCut task");
    println!("Goal  {}", task.goal);
    println!("Acceptance  {}", task.acceptance.join(", "));

    if let Some(failure) = &task.current_failure {
        println!("Current failure  {}: {}", failure.kind, failure.summary);
        for location in &failure.locations {
            println!("  {location}");
        }
    } else {
        println!("Current failure  none");
    }

    println!("Known evidence");
    if task.observations.is_empty() {
        println!("  none");
    } else {
        for observation in &task.observations {
            println!(
                "  {}  {}  raw {} tokens -> packet {} tokens",
                observation.id,
                observation.summary,
                format_count(observation.tokens.raw),
                format_count(observation.tokens.packet)
            );
        }
    }

    println!("Open hypotheses");
    for hypothesis in task
        .hypotheses
        .iter()
        .filter(|hypothesis| hypothesis.status == "open")
    {
        println!("  {}  {}", hypothesis.id, hypothesis.summary);
        println!("      confidence: {}", hypothesis.confidence);
    }

    println!("Suggested next actions");
    if task.next_actions.is_empty() {
        println!("  none");
    } else {
        for (index, action) in task.next_actions.iter().enumerate() {
            println!("  {}. {}", index + 1, action.command);
            println!("     reason: {}", action.reason);
            println!("     expected answer: {}", action.expected_answer);
            println!("     estimated cost: {} tokens", action.estimated_tokens);
        }
    }

    println!(
        "Budget  packet tokens used: {} / {} soft",
        format_count(task.budget.packet_tokens_used),
        format_count(task.budget.soft_tokens)
    );
    println!(
        "        raw tokens avoided: {}",
        format_count(task.budget.raw_tokens_avoided)
    );
}

fn task_id() -> String {
    let timestamp = Utc::now().format("%Y-%m-%dT%H%M%SZ");
    let suffix = Uuid::new_v4().simple().to_string();
    format!("task-{timestamp}-{}", &suffix[..6])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{LikelyFailure, Outcome, TokenSummary};

    #[test]
    fn derives_assertion_literal_hypothesis() {
        let evidence = EvidencePacket {
            schema_version: 1,
            run_id: "run1".to_string(),
            outcome: Outcome {
                exit_code: 101,
                status: "failed".to_string(),
            },
            likely_failure: Some(LikelyFailure {
                kind: "test_failure".to_string(),
                summary: "test config::tests::example failed".to_string(),
                confidence: "high".to_string(),
            }),
            primary_diagnostic: None,
            diagnostics: vec![crate::evidence::EvidenceDiagnostic {
                kind: DiagnosticKind::Panic,
                severity: crate::extract::Severity::Error,
                code: None,
                message: "assertion failed: error.to_string().contains(\"already existsabc\")"
                    .to_string(),
                file: Some("src/config.rs".to_string()),
                line: Some(213),
                column: Some(9),
            }],
            file_refs: Vec::new(),
            context_items: Vec::new(),
            token_summary: TokenSummary {
                raw_tokens: 1913,
                packet_tokens: 362,
                saved_tokens: 1551,
                reduction_percent: 81.1,
            },
        };
        let observation = Observation {
            id: "obs1".to_string(),
            source: "run:run1".to_string(),
            kind: "test_failure".to_string(),
            summary: "failed".to_string(),
            locations: Vec::new(),
            tokens: ObservationTokens {
                raw: 1913,
                packet: 362,
            },
        };

        let hypotheses = hypotheses_for_observation(&evidence, &observation);

        assert_eq!(hypotheses.len(), 1);
        assert!(hypotheses[0].summary.contains("already existsabc"));
        assert_eq!(hypotheses[0].confidence, "high");
    }
}
