//! `haycut agent session`: a line-oriented terminal REPL driven entirely
//! through `AgentEngine`/`ControlCommand`/`AgentEvent`. Intentionally
//! boring — line in, events out — no terminal UI complexity.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use crate::cli::TaskTarget;
use crate::commands::agent::engine::{
    AgentEngine, AgentEvent, ContextTarget, ControlCommand, VerificationCommand,
};
use crate::commands::task::{self, TaskState};

pub fn run(task_target: Option<TaskTarget>, goal: String) -> i32 {
    if task_target != Some(TaskTarget::Current) && goal.trim().is_empty() {
        eprintln!("Error: provide --task current or a goal");
        return 2;
    }

    if !goal.trim().is_empty() && task_target != Some(TaskTarget::Current) {
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

    let mut engine = AgentEngine::new();
    let stdin = io::stdin();

    // Run autonomously until the first blocking point, same as typing
    // `continue` immediately.
    if let Err(error) = drive(&mut engine, &mut task, ControlCommand::Continue) {
        eprintln!("Error running agent step: {error}");
        return 1;
    }

    loop {
        if task.terminal_reason.is_some() {
            return 0;
        }

        print!("\nhaycut> ");
        if io::stdout().flush().is_err() {
            return 1;
        }

        let mut line = String::new();
        let bytes_read = match stdin.lock().read_line(&mut line) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("Error reading input: {error}");
                return 1;
            }
        };
        if bytes_read == 0 {
            // EOF: persist as an explicit stop rather than losing state.
            let _ = drive(&mut engine, &mut task, ControlCommand::Stop);
            return 0;
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        match parse_command(line, &task) {
            ReplInput::Command(command) => {
                let stop_requested = matches!(command, ControlCommand::Stop);
                if let Err(error) = drive(&mut engine, &mut task, command) {
                    eprintln!("Error running agent step: {error}");
                    return 1;
                }
                if stop_requested {
                    return 0;
                }
            }
            ReplInput::Status => print_status(&task),
            ReplInput::Trace => print_trace(&task),
            ReplInput::Unknown(input) => {
                println!("Unrecognized command: {input}");
                println!(
                    "Known commands: continue, step, approve, reject <reason>, steer <instruction>, context <path-or-symbol>, verify <command>, status, trace, stop"
                );
            }
        }
    }
}

enum ReplInput {
    Command(ControlCommand),
    Status,
    Trace,
    Unknown(String),
}

fn parse_command(line: &str, task: &TaskState) -> ReplInput {
    let (word, rest) = match line.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (line, ""),
    };

    match word {
        "continue" => ReplInput::Command(ControlCommand::Continue),
        "step" => ReplInput::Command(ControlCommand::Step),
        "approve" => ReplInput::Command(ControlCommand::Approve),
        "reject" => ReplInput::Command(ControlCommand::Reject {
            reason: rest.to_string(),
        }),
        "steer" => ReplInput::Command(ControlCommand::Steer {
            message: rest.to_string(),
        }),
        "context" => ReplInput::Command(ControlCommand::AddContext {
            target: parse_context_target(rest),
        }),
        "verify" => ReplInput::Command(ControlCommand::Verify {
            command: parse_verify_command(rest),
        }),
        "status" => ReplInput::Status,
        "trace" => ReplInput::Trace,
        "stop" => ReplInput::Command(ControlCommand::Stop),
        _ if task.pending_interaction.is_some() => ReplInput::Command(ControlCommand::Reply {
            message: line.to_string(),
        }),
        _ => ReplInput::Unknown(line.to_string()),
    }
}

fn parse_context_target(input: &str) -> ContextTarget {
    if let Some((path, symbol)) = input.split_once("::") {
        return ContextTarget::Symbol(format!("{path}::{symbol}"));
    }
    if let Some((path, line)) = input.rsplit_once(':')
        && let Ok(line) = line.parse::<usize>()
    {
        return ContextTarget::Window {
            path: PathBuf::from(path),
            line,
        };
    }
    ContextTarget::Search(input.to_string())
}

fn parse_verify_command(input: &str) -> VerificationCommand {
    VerificationCommand::parse(input).unwrap_or(VerificationCommand {
        program: String::new(),
        args: Vec::new(),
    })
}

fn drive(
    engine: &mut AgentEngine,
    task: &mut TaskState,
    command: ControlCommand,
) -> io::Result<()> {
    let events = engine.advance(task, command)?;
    for event in &events {
        print_event(event);
    }
    task::save_current(task)
}

fn print_event(event: &AgentEvent) {
    match event {
        AgentEvent::NodeStarted { node_id, op } => {
            println!("[agent] started {} ({node_id})", op.name())
        }
        AgentEvent::Progress(summary) => println!("[agent] {summary}"),
        AgentEvent::ActionProposed(action) => {
            println!("[agent] wants to {}", describe_action(action))
        }
        AgentEvent::ApprovalRequired(request) => {
            println!("[agent] proposes changes:\n{}", request.summary);
        }
        AgentEvent::Question(interaction) => println!("[agent] {}", interaction.question),
        AgentEvent::VerificationCompleted { summary } => println!("[verify] {summary}"),
        AgentEvent::PatchProposed { summary } => println!("[agent] proposes patch:\n{summary}"),
        AgentEvent::Finished(outcome) => println!("[finished] {}", outcome.summary),
        AgentEvent::Stopped(reason) => println!("[stopped] {reason:?}"),
    }
}

fn describe_action(action: &crate::commands::agent::AgentAction) -> String {
    use crate::commands::agent::AgentAction;
    match action {
        AgentAction::Search { query } => format!("search for `{query}`"),
        AgentAction::ReadSymbol { target } => format!("read symbol {target}"),
        AgentAction::ReadWindow { path, line, .. } => format!("read {}:{line}", path.display()),
        AgentAction::RunCommand { program, args } => format!("run `{program} {}`", args.join(" ")),
        AgentAction::PullContext { id } => format!("pull context {id}"),
        AgentAction::PlanPatch => "propose edits".to_string(),
        AgentAction::AskUser { question } => format!("ask: {question}"),
        AgentAction::Finish => "finish".to_string(),
    }
}

fn print_status(task: &TaskState) {
    println!("Goal  {}", task.goal);
    println!(
        "Intent  {}",
        task.intent
            .map(|intent| format!("{intent:?}"))
            .unwrap_or_else(|| "unknown".to_string())
    );
    println!(
        "Current failure  {}",
        task.current_failure
            .as_ref()
            .map(|failure| failure.summary.clone())
            .unwrap_or_else(|| "none".to_string())
    );
    println!(
        "Constraints  {}",
        if task.constraints.is_empty() {
            "none".to_string()
        } else {
            task.constraints.join("; ")
        }
    );
    println!(
        "Pending interaction  {}",
        task.pending_interaction
            .as_ref()
            .map(|interaction| interaction.question.clone())
            .unwrap_or_else(|| "none".to_string())
    );
    println!(
        "Pending approval  {}",
        task.pending_approval
            .as_ref()
            .map(|approval| approval.summary.clone())
            .unwrap_or_else(|| "none".to_string())
    );
    println!(
        "Budget  {} / {} packet tokens",
        task.budget.packet_tokens_used, task.budget.soft_tokens
    );
}

fn print_trace(task: &TaskState) {
    if task.route.is_empty() {
        println!("(no steps recorded)");
        return;
    }
    for entry in task
        .route
        .iter()
        .rev()
        .take(10)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        println!(
            "  {}({}) -> {}",
            entry.step,
            entry.executor.name(),
            entry.outcome
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent::workflow::Workflow;
    use crate::commands::task::{TaskBudget, TaskIntent};

    fn empty_task() -> TaskState {
        TaskState {
            schema_version: 1,
            id: "task-repl-test".to_string(),
            title: "test".to_string(),
            goal: "test goal".to_string(),
            acceptance: Vec::new(),
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
            pending_agent_action: None,
            intent: Some(TaskIntent::DebugFailure),
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
            patch_approval_granted: false,
            command_approval_granted: false,
            terminal_reason: None,
            retry_count: 0,
            last_failure_signature: None,
            available_context: Vec::new(),
            workflow_spec: None,
            workflow: Workflow::new(),
            pending_interaction: None,
            pending_approval: None,
            messages: Vec::new(),
            explicit_verify_commands: Vec::new(),
            inspected_digests: Default::default(),
            verification_results: Vec::new(),
        }
    }

    #[test]
    fn parses_reject_with_reason() {
        let task = empty_task();
        match parse_command("reject The public API must remain unchanged", &task) {
            ReplInput::Command(ControlCommand::Reject { reason }) => {
                assert_eq!(reason, "The public API must remain unchanged");
            }
            _ => panic!("expected Reject command"),
        }
    }

    #[test]
    fn parses_steer_with_message() {
        let task = empty_task();
        match parse_command("steer Check whether the counter can overflow", &task) {
            ReplInput::Command(ControlCommand::Steer { message }) => {
                assert_eq!(message, "Check whether the counter can overflow");
            }
            _ => panic!("expected Steer command"),
        }
    }

    #[test]
    fn parses_context_symbol_target() {
        match parse_context_target("src/cache.rs::Cache") {
            ContextTarget::Symbol(target) => assert_eq!(target, "src/cache.rs::Cache"),
            _ => panic!("expected Symbol target"),
        }
    }

    #[test]
    fn parses_context_window_target() {
        match parse_context_target("src/cache.rs:184") {
            ContextTarget::Window { path, line } => {
                assert_eq!(path, PathBuf::from("src/cache.rs"));
                assert_eq!(line, 184);
            }
            _ => panic!("expected Window target"),
        }
    }

    #[test]
    fn free_text_becomes_reply_when_blocked_on_a_question() {
        let mut task = empty_task();
        task.pending_interaction = Some(crate::commands::agent::engine::PendingInteraction {
            question: "Which behaviour is correct?".to_string(),
            asked_at: chrono::Utc::now(),
        });
        match parse_command("the old one, it predates the refactor", &task) {
            ReplInput::Command(ControlCommand::Reply { message }) => {
                assert_eq!(message, "the old one, it predates the refactor");
            }
            _ => panic!("expected Reply command"),
        }
    }

    #[test]
    fn unrecognized_word_without_pending_question_is_unknown() {
        let task = empty_task();
        assert!(matches!(
            parse_command("frobnicate", &task),
            ReplInput::Unknown(_)
        ));
    }
}
