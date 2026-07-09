use std::{collections::BTreeMap, io, path::Path, sync::OnceLock};

use chrono::Utc;
use minijinja::{Environment, context};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    cli::{AgentCommand, TaskTarget},
    commands::{read_symbol, read_window, search, task, trace},
    config::Config,
    model::{EstimatedTokenUsage, ModelProvider, ModelPurpose, ModelRequest, OpenAiProvider, ToolDefinition},
    store::{self, NewAgentTrace, RUN_STORE_PATH},
    util::{estimate_tokens, format_count},
};

pub const DEFAULT_MAX_STEPS: usize = 8;
const SEARCH_LIMIT: usize = 20;
const MAX_OUTPUT_TOKENS: usize = 512;

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
            verify,
            max_steps,
            goal,
        } => run_loop(task, goal.join(" "), verify, max_steps),
        AgentCommand::Step { task } => run_step(task),
        AgentCommand::Trace { task } => run_trace(task),
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
}

#[derive(Debug)]
struct StepOutcome {
    action: PlannerAction,
    observation: String,
    done: bool,
    blocked: bool,
}

fn run_loop(
    task_target: Option<TaskTarget>,
    goal: String,
    verify: Option<String>,
    max_steps: usize,
) -> i32 {
    if task_target != Some(TaskTarget::Current) && goal.trim().is_empty() {
        eprintln!("Error: provide --task current or a goal");
        return 2;
    }

    if !goal.trim().is_empty() && task_target != Some(TaskTarget::Current) {
        match task::start_current(goal, verify) {
            Ok(task) => println!("Started task {}", task.id),
            Err(error) => {
                eprintln!("Error starting task: {error}");
                return 1;
            }
        }
    }

    for step in 0..max_steps {
        match step_once() {
            Ok(outcome) => {
                println!(
                    "step {}: {:?} — {}",
                    step + 1,
                    outcome.action.action,
                    first_line(&outcome.observation)
                );
                if outcome.done || outcome.blocked {
                    return 0;
                }
            }
            Err(error) => {
                eprintln!("Error running agent step: {error}");
                return 1;
            }
        }
    }

    println!("Stopped after {max_steps} steps.");
    0
}

fn run_step(task_target: Option<TaskTarget>) -> i32 {
    if task_target != Some(TaskTarget::Current) {
        eprintln!("Error: v0 supports `haycut agent step --task current`");
        return 2;
    }

    match step_once() {
        Ok(outcome) => {
            println!("Selected action: {:?}", outcome.action.action);
            println!("Reason: {}", outcome.action.reason);
            println!("Observation: {}", outcome.observation);
            0
        }
        Err(error) => {
            eprintln!("Error running agent step: {error}");
            1
        }
    }
}

fn run_trace(task_target: Option<TaskTarget>) -> i32 {
    if task_target != Some(TaskTarget::Current) {
        eprintln!("Error: v0 supports `haycut agent trace --task current`");
        return 2;
    }

    match task::load_current()
        .and_then(|task| store::agent_traces_for_task(Path::new(RUN_STORE_PATH), &task.id))
    {
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

fn step_once() -> io::Result<StepOutcome> {
    let mut task = task::load_current()?;
    let Some(model_config) = Config::load_model()
        .map_err(|error| io::Error::other(error.to_string()))?
    else {
        let path_hint = crate::config::UserConfig::path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "~/.config/haycut/config.toml".to_string());
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("agent step requires [model] configuration in {path_hint}"),
        ));
    };
    let provider = OpenAiProvider::new(model_config);

    let step_index = next_step_index(&task);

    // Deterministic step-0 shortcut: on the first step of a fresh task, use the
    // cheap triage model to classify intent. For debug tasks with a known test
    // command, reproduce the failure directly instead of spending a full
    // planner turn deciding to. Any uncertainty defers to the planner.
    if step_index == 1 && task.intent.is_none() && task.current_failure.is_none() {
        if let Some(outcome) = try_reproduce_first(&mut task, step_index)? {
            return Ok(outcome);
        }
    }

    let prompt = planner_prompt(&task);
    let request = model_request(&task, &prompt);
    let estimated = request.estimated_tokens;
    let (tool_name, args_value, response) = provider
        .complete_with_tools(request, &planner_tools())
        .map_err(|error| io::Error::other(error.to_string()))?;
    let action = action_from_tool_call(&tool_name, args_value)?;
    validate_action(&action, &task)?;
    let observation = execute_action(&action)?;

    task.observations.push(task::Observation {
        id: format!("obs{}", task.observations.len() + 1),
        source: format!("agent:step:{step_index}"),
        kind: format!("agent_{:?}", action.action).to_lowercase(),
        summary: observation.clone(),
        locations: action_locations(&action),
        tokens: task::ObservationTokens {
            raw: estimate_tokens(observation.as_bytes()),
            packet: estimate_tokens(observation.as_bytes()),
        },
    });
    task.budget.packet_tokens_used = task
        .budget
        .packet_tokens_used
        .saturating_add(estimated.input)
        .saturating_add(response.reported_tokens.output.unwrap_or(estimated.output));
    task.next_actions.clear();
    task::save_current(&task)?;

    let action_json = serde_json::to_string(&action).map_err(io::Error::other)?;
    store::insert_agent_trace(
        Path::new(RUN_STORE_PATH),
        &NewAgentTrace {
            id: &trace_id(),
            task_id: &task.id,
            step_index: step_index as i64,
            prompt: &prompt,
            response: &response.text,
            action_json: &action_json,
            observation: &observation,
            estimated_input_tokens: estimated.input as i64,
            estimated_output_tokens: estimated.output as i64,
            reported_input_tokens: response.reported_tokens.input.map(|value| value as i64),
            reported_output_tokens: response.reported_tokens.output.map(|value| value as i64),
            created_at: &Utc::now().to_rfc3339(),
        },
    )?;

    Ok(StepOutcome {
        done: action.action == ActionKind::Finish,
        blocked: action.action == ActionKind::AskUser,
        action,
        observation,
    })
}

/// Attempt the deterministic step-0 reproduce. Returns `Ok(Some(_))` when it
/// classified the task as a failure to debug and reproduced it directly (so the
/// caller should return without a planner call), or `Ok(None)` to fall through
/// to the normal planner. Never hard-fails the step: any triage error or
/// missing signal defers to the planner.
fn try_reproduce_first(
    task: &mut task::TaskState,
    step_index: usize,
) -> io::Result<Option<StepOutcome>> {
    // Without a known test command there is nothing deterministic to run.
    let Some(env) = detect_project_env(Path::new(".")) else {
        return Ok(None);
    };

    let Some(triage_config) =
        Config::load_triage_model().map_err(|error| io::Error::other(error.to_string()))?
    else {
        return Ok(None);
    };
    let triage = OpenAiProvider::new(triage_config);

    let (intent, input_tokens, output_tokens) = match classify_task(&triage, &task.goal) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("triage classification failed; deferring to planner: {error}");
            return Ok(None);
        }
    };

    if !intent.reproduce_first() {
        // Persist the verdict so we never reclassify, then let the planner run.
        task.intent = Some(intent);
        task.budget.packet_tokens_used = task
            .budget
            .packet_tokens_used
            .saturating_add(input_tokens)
            .saturating_add(output_tokens);
        task::save_current(task)?;
        return Ok(None);
    }

    // Reproduce through the normal trace pipeline; it captures gated evidence
    // and sets `current_failure` on the persisted task.
    let command: Vec<String> = env
        .test_command
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let exit_code = trace::run(command, None, Some(TaskTarget::Current));

    // `trace::run` rewrote the on-disk task; reload before layering on the
    // triage verdict and its token cost.
    let mut task = task::load_current()?;
    task.intent = Some(intent);
    task.budget.packet_tokens_used = task
        .budget
        .packet_tokens_used
        .saturating_add(input_tokens)
        .saturating_add(output_tokens);
    task::save_current(&task)?;

    let observation = format!(
        "triage=debug_failure; reproduced `{}` (exit {exit_code})",
        env.test_command
    );

    store::insert_agent_trace(
        Path::new(RUN_STORE_PATH),
        &NewAgentTrace {
            id: &trace_id(),
            task_id: &task.id,
            step_index: step_index as i64,
            prompt: "<deterministic triage + reproduce>",
            response: "debug_failure",
            action_json: "{\"action\":\"trace\",\"deterministic\":true}",
            observation: &observation,
            estimated_input_tokens: input_tokens as i64,
            estimated_output_tokens: output_tokens as i64,
            reported_input_tokens: Some(input_tokens as i64),
            reported_output_tokens: Some(output_tokens as i64),
            created_at: &Utc::now().to_rfc3339(),
        },
    )?;

    Ok(Some(StepOutcome {
        action: PlannerAction {
            action: ActionKind::Trace,
            args: ActionArgs {
                command: Some(env.test_command.clone()),
                ..Default::default()
            },
            reason: "deterministic reproduce (triage classified debug_failure)".to_string(),
        },
        observation,
        done: false,
        blocked: false,
    }))
}

/// Cheap, single-purpose classifier. Sends only the goal and a small enum of
/// intents to the triage model — no tool schemas, no history — so it stays a
/// low-cost triage call rather than a full planner turn.
fn classify_task(
    provider: &OpenAiProvider,
    goal: &str,
) -> io::Result<(task::TaskIntent, usize, usize)> {
    let prompt = format!("Classify this software task into exactly one intent.\nTask: {goal}");
    let input_estimate = estimate_tokens(prompt.as_bytes());
    let request = ModelRequest {
        purpose: ModelPurpose::TaskTriage,
        system: None,
        prompt,
        estimated_tokens: EstimatedTokenUsage {
            input: input_estimate,
            output: 16,
        },
        max_output_tokens: Some(16),
        metadata: BTreeMap::new(),
    };

    let (_tool, args, response) = provider
        .complete_with_tools(request, &classifier_tools())
        .map_err(|error| io::Error::other(error.to_string()))?;

    let intent = parse_intent(args.get("intent").and_then(|value| value.as_str()));
    let input = response.reported_tokens.input.unwrap_or(input_estimate);
    let output = response.reported_tokens.output.unwrap_or(16);
    Ok((intent, input, output))
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
    vec![ToolDefinition {
        name: "classify",
        description: "Record the single best-fitting intent for the task.",
        parameters: serde_json::json!({
            "type": "object",
            "required": ["intent"],
            "additionalProperties": false,
            "properties": {
                "intent": {
                    "type": "string",
                    "enum": [
                        "debug_failure",
                        "implement_feature",
                        "refactor",
                        "answer_question"
                    ]
                }
            }
        }),
    }]
}

fn planner_tools() -> Vec<ToolDefinition> {
    // Tool names are kept short to minimise schema token cost.
    // Each tool only carries the args it actually needs.
    vec![
        ToolDefinition {
            name: "search",
            description: "Exact-string search across the repo; use to locate symbols, call sites or error text.",
            parameters: serde_json::json!({
                "type": "object",
                "required": ["query"],
                "additionalProperties": false,
                "properties": {
                    "query":  { "type": "string" }
                }
            }),
        },
        ToolDefinition {
            name: "sym",
            description: "Read one parsed symbol (name or path::name) with its body; prefer when the symbol is known.",
            parameters: serde_json::json!({
                "type": "object",
                "required": ["symbol"],
                "additionalProperties": false,
                "properties": {
                    "symbol": { "type": "string" }
                }
            }),
        },
        ToolDefinition {
            name: "win",
            description: "Read a small line window around a file:line; use when no symbol name is known.",
            parameters: serde_json::json!({
                "type": "object",
                "required": ["file", "line"],
                "additionalProperties": false,
                "properties": {
                    "file":   { "type": "string" },
                    "line":   { "type": "integer" },
                    "radius": { "type": "integer" }
                }
            }),
        },
        ToolDefinition {
            name: "trace",
            description: "Run a command and capture gated output; use to reproduce failures and verify fixes.",
            parameters: serde_json::json!({
                "type": "object",
                "required": ["command"],
                "additionalProperties": false,
                "properties": {
                    "command": { "type": "string" },
                    "args":    { "type": "array", "items": { "type": "string" } }
                }
            }),
        },
        ToolDefinition {
            name: "plan",
            description: "Enough context exists to write the patch; reason is the concise patch plan.",
            parameters: serde_json::json!({
                "type": "object",
                "required": ["reason"],
                "additionalProperties": false,
                "properties": {
                    "reason": { "type": "string" }
                }
            }),
        },
        ToolDefinition {
            name: "finish",
            description: "Task complete and verified; reason is the outcome summary.",
            parameters: serde_json::json!({
                "type": "object",
                "required": ["reason"],
                "additionalProperties": false,
                "properties": {
                    "reason": { "type": "string" }
                }
            }),
        },
        ToolDefinition {
            name: "ask",
            description: "Ask the user one question, only when no tool or the repo can answer it.",
            parameters: serde_json::json!({
                "type": "object",
                "required": ["question"],
                "additionalProperties": false,
                "properties": {
                    "question": { "type": "string" }
                }
            }),
        },
    ]
}

/// Map a tool-call result back to the internal `PlannerAction` representation.
fn action_from_tool_call(
    tool: &str,
    args: serde_json::Value,
) -> io::Result<PlannerAction> {
    let str_field = |key: &str| -> Option<String> {
        args.get(key).and_then(|v| v.as_str()).map(str::to_string)
    };
    let usize_field = |key: &str| -> Option<usize> {
        args.get(key).and_then(|v| v.as_u64()).map(|n| n as usize)
    };
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
            ActionArgs { query: str_field("query"), ..Default::default() },
        ),
        "sym" => (
            ActionKind::ReadSymbol,
            ActionArgs { symbol: str_field("symbol"), ..Default::default() },
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
            ActionArgs { question: str_field("question"), ..Default::default() },
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
            environment => detect_project_env(Path::new(".")),
            failure => task.current_failure,
            observations => observations,
            hypotheses => hypotheses,
            budget => context! {
                used => task.budget.packet_tokens_used,
                soft => task.budget.soft_tokens,
                hard => task.budget.hard_tokens,
            },
        })
        .expect("planner prompt renders")
}

fn model_request(task: &task::TaskState, prompt: &str) -> ModelRequest {
    let mut metadata = BTreeMap::new();
    metadata.insert("task_id".to_string(), task.id.clone());

    ModelRequest {
        purpose: ModelPurpose::AgentPlanner,
        system: Some(PLANNER_SYSTEM_PROMPT.to_string()),
        prompt: prompt.to_string(),
        estimated_tokens: EstimatedTokenUsage {
            input: estimate_tokens(PLANNER_SYSTEM_PROMPT.as_bytes())
                + estimate_tokens(prompt.as_bytes()),
            output: MAX_OUTPUT_TOKENS,
        },
        max_output_tokens: Some(MAX_OUTPUT_TOKENS),
        metadata,
    }
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
    }
}

fn execute_action(action: &PlannerAction) -> io::Result<String> {
    match action.action {
        ActionKind::Search => execute_search(action.args.query.as_deref().unwrap_or_default()),
        ActionKind::ReadSymbol => {
            execute_read_symbol(action.args.symbol.as_deref().unwrap_or_default())
        }
        ActionKind::ReadWindow => execute_read_window(
            action.args.file.as_deref().unwrap_or_default(),
            action.args.line.unwrap_or(1),
            action.args.radius.unwrap_or(read_window::DEFAULT_RADIUS),
        ),
        ActionKind::Trace => execute_trace(action),
        ActionKind::ProposePatchPlan => Ok(if action.reason.trim().is_empty() {
            "planner says patch context is sufficient".to_string()
        } else {
            action.reason.clone()
        }),
        ActionKind::Finish => Ok(action.reason.clone()),
        ActionKind::AskUser => Ok(action
            .args
            .question
            .as_deref()
            .unwrap_or_default()
            .to_string()),
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

fn execute_read_symbol(symbol: &str) -> io::Result<String> {
    let item = read_symbol::read_symbol(Path::new("."), symbol)?;
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

fn execute_read_window(file: &str, line: usize, radius: usize) -> io::Result<String> {
    let window = read_window::read_window(file.into(), line, radius, false)?;
    Ok(truncate(&window.render(), 2_000))
}

fn execute_trace(action: &PlannerAction) -> io::Result<String> {
    let mut command = Vec::new();
    command.push(action.args.command.clone().unwrap_or_default());
    command.extend(action.args.args.clone());
    let exit_code = trace::run(command, None, Some(TaskTarget::Current));
    Ok(format!("trace exited with code {exit_code}"))
}

fn action_locations(action: &PlannerAction) -> Vec<String> {
    match action.action {
        ActionKind::ReadSymbol => action.args.symbol.iter().cloned().collect(),
        ActionKind::ReadWindow => action
            .args
            .file
            .as_ref()
            .zip(action.args.line)
            .map(|(file, line)| vec![format!("{file}:{line}")])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn next_step_index(task: &task::TaskState) -> usize {
    task.observations
        .iter()
        .filter(|observation| observation.source.starts_with("agent:step:"))
        .count()
        + 1
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
            intent: None,
            current_failure: Some(task::CurrentFailure {
                kind: "test_failure".to_string(),
                summary: "config test failed".to_string(),
                locations: vec!["src/config.rs:213:9".to_string()],
            }),
            closed_at: None,
        }
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

        assert!(!prompt.contains("\n\n"), "prompt had a blank line:\n{prompt}");
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
        assert_eq!(parse_intent(Some("nonsense")), task::TaskIntent::AnswerQuestion);
        assert_eq!(parse_intent(None), task::TaskIntent::AnswerQuestion);
    }

    #[test]
    fn only_debug_failure_reproduces_first() {
        assert!(task::TaskIntent::DebugFailure.reproduce_first());
        assert!(!task::TaskIntent::ImplementFeature.reproduce_first());
        assert!(!task::TaskIntent::Refactor.reproduce_first());
        assert!(!task::TaskIntent::AnswerQuestion.reproduce_first());
    }

    #[test]
    fn action_from_tool_call_maps_sym_to_read_symbol() {
        let args =
            serde_json::json!({ "symbol": "create_default_config_at", "reason": "inspect" });
        let action = action_from_tool_call("sym", args).expect("should map");

        assert_eq!(action.action, ActionKind::ReadSymbol);
        assert_eq!(action.args.symbol.as_deref(), Some("create_default_config_at"));
        assert_eq!(action.reason, "inspect");
    }

    #[test]
    fn action_from_tool_call_rejects_unknown_tool() {
        let error = action_from_tool_call("bogus", serde_json::json!({})).unwrap_err();
        assert!(error.to_string().contains("unknown tool"));
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
}
