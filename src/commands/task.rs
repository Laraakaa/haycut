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
    /// Cheap-classifier verdict of what kind of work this task is. Set once on
    /// the first agent step and used to route deterministic shortcuts (e.g.
    /// reproduce-first for debug tasks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<TaskIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_failure: Option<CurrentFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
}

/// Coarse task classification produced by the triage model.
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

impl TaskIntent {
    /// Whether HayCut should deterministically run the verification command
    /// before the first planner call for a task with this intent.
    pub fn reproduce_first(self) -> bool {
        matches!(self, TaskIntent::DebugFailure)
    }
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
}

pub fn attach_current_run(
    trace: &CommandTrace,
    packet: &CompactPacket,
    evidence: &EvidencePacket,
) -> io::Result<()> {
    let mut task = load_current()?;
    let run_id = evidence.run_id.clone();

    if !task.runs.iter().any(|run| run.id == run_id) {
        task.runs.push(TaskRun {
            id: run_id.clone(),
            command: packet.command.clone(),
            exit_code: trace.exit_code,
            raw_tokens: packet.raw_tokens,
            packet_tokens: packet.packet_tokens,
        });
    }

    task.budget.packet_tokens_used = task.runs.iter().map(|run| run.packet_tokens).sum();
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

    let task = TaskState {
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
        intent: None,
        current_failure: None,
        closed_at: None,
    };

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

fn function_under_test(evidence: &EvidencePacket, cwd: &Path) -> Option<String> {
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

fn function_call_name(line: &str) -> Option<String> {
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
