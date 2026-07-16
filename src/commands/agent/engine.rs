//! Engine-facing control API: `AgentEvent`s out, `ControlCommand`s in.
//!
//! This is the single contract the CLI, terminal REPL, dashboard, and any
//! future MCP-style interface should drive the agent through, instead of
//! each caller re-implementing the step/decide loop in `agent.rs`.

use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::commands::agent::workflow::{self, Decision, NodeOp, StopReason};
use crate::commands::agent::{AgentAction, execute_step, record_route};
use crate::commands::task::{self, TaskState};
/// Re-exported so callers (REPL, dashboard) use the same verification
/// command type the structured `VerificationPlan` is built from, instead of
/// engine.rs maintaining its own duplicate shape.
pub use crate::commands::task::VerificationCommand;

const MAX_RETRIES: usize = 2;

/// One durable message in a task's interaction transcript.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TaskMessage {
    pub role: MessageRole,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Agent,
}

/// A question the workflow is blocked on until the user replies.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PendingInteraction {
    pub question: String,
    pub asked_at: DateTime<Utc>,
}

/// A proposed mutation awaiting explicit user approval or rejection.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ApprovalRequest {
    pub summary: String,
}

/// A target for `ControlCommand::AddContext`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum ContextTarget {
    Symbol(String),
    Window { path: PathBuf, line: usize },
    Search(String),
}

/// Final, terminal outcome of a task.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TaskOutcome {
    pub summary: String,
    pub patch_applied: bool,
}

/// Human- or external-facing command vocabulary.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum ControlCommand {
    Continue,
    Step,
    Approve,
    Reject { reason: String },
    Steer { message: String },
    AddContext { target: ContextTarget },
    Verify { command: VerificationCommand },
    Reply { message: String },
    Stop,
}

/// Engine-facing event stream. The CLI, future TUI, dashboard controller,
/// and MCP-style interface should all consume the same events.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum AgentEvent {
    Progress(String),
    ActionProposed(AgentAction),
    ApprovalRequired(ApprovalRequest),
    Question(PendingInteraction),
    VerificationCompleted { summary: String },
    PatchProposed { summary: String },
    Finished(TaskOutcome),
    Stopped(StopReason),
}

/// Stateless driver over the existing step/decide loop. All persistent
/// state lives on `TaskState` (saved by the caller), so `advance()` and
/// `run_until_blocked()` operate directly on an in-memory task and never
/// touch storage themselves — callers own load/save, matching every other
/// entry point in `agent.rs`.
pub struct AgentEngine;

impl AgentEngine {
    pub fn new() -> Self {
        AgentEngine
    }

    /// Execute exactly one control decision and return the events it produced.
    pub fn advance(&mut self, task: &mut TaskState, command: ControlCommand) -> io::Result<Vec<AgentEvent>> {
        match command {
            ControlCommand::Continue => self.run_until_blocked(task),
            ControlCommand::Step => Ok(vec![self.advance_one(task)?]),
            ControlCommand::Approve => Ok(self.approve(task)),
            ControlCommand::Reject { reason } => Ok(self.reject(task, reason)),
            ControlCommand::Steer { message } => Ok(self.steer(task, message)),
            ControlCommand::Reply { message } => Ok(self.reply(task, message)),
            ControlCommand::AddContext { target } => self.add_context(task, target),
            ControlCommand::Verify { command } => self.verify(task, command),
            ControlCommand::Stop => Ok(vec![self.stop(task)]),
        }
    }

    /// Continue until the task needs human input, approval, a risky command
    /// decision, or has finished.
    pub fn run_until_blocked(&mut self, task: &mut TaskState) -> io::Result<Vec<AgentEvent>> {
        let mut events = Vec::new();
        loop {
            let event = self.advance_one(task)?;
            let blocked = matches!(
                event,
                AgentEvent::ApprovalRequired(_)
                    | AgentEvent::Question(_)
                    | AgentEvent::Finished(_)
                    | AgentEvent::Stopped(_)
            );
            events.push(event);
            if blocked {
                break;
            }
        }
        Ok(events)
    }

    /// Execute exactly one engine advance: either ask the graph what's next
    /// and run it, or interpret a stop decision into a specific event.
    fn advance_one(&mut self, task: &mut TaskState) -> io::Result<AgentEvent> {
        let mut workflow = task.workflow.clone();
        let (node_id, next) = match workflow::next_ready_node(&mut workflow, task, MAX_RETRIES) {
            Decision::Stop(reason) => return Ok(interpret_stop(task, reason)),
            Decision::Ready(id, op) => (id, op),
        };
        workflow.mark_running(&node_id);
        task.workflow = workflow;

        let step_index = task.route.len() + 1;
        let action_event = if next == NodeOp::PlanContext {
            task.pending_agent_action.clone().map(AgentEvent::ActionProposed)
        } else {
            None
        };

        let outcome = match execute_step(&next, task, step_index) {
            Ok(outcome) => outcome,
            Err(error) => {
                task.workflow.mark_failed(&node_id);
                return Err(error);
            }
        };
        task.workflow.complete(node_id, next, outcome.summary.clone());
        record_route(task, &next, &outcome);

        if next == NodeOp::AskUser {
            let question = match &task.pending_agent_action {
                Some(AgentAction::AskUser { question }) => question.clone(),
                _ => outcome.summary.clone(),
            };
            let interaction = PendingInteraction { question: question.clone(), asked_at: Utc::now() };
            task.pending_interaction = Some(interaction);
            task.messages.push(TaskMessage {
                role: MessageRole::Agent,
                content: question,
                created_at: Utc::now(),
            });
        }
        if next == NodeOp::ApplyPatch && task.patch_previewed && !task.patch_applied {
            task.pending_approval = Some(ApprovalRequest { summary: outcome.summary.clone() });
        }

        // `ActionProposed` for context/patch/finish/ask decisions is more
        // informative than a bare progress line, so surface it in place of
        // the PlanContext step's own progress event.
        if let Some(action_event) = action_event {
            return Ok(action_event);
        }

        Ok(AgentEvent::Progress(format!("{}: {}", next.name(), outcome.summary)))
    }

    fn approve(&mut self, task: &mut TaskState) -> Vec<AgentEvent> {
        task.pending_approval = None;
        task.apply_requested = true;
        task.patch_previewed = false;
        vec![AgentEvent::Progress("approved pending patch".to_string())]
    }

    fn reject(&mut self, task: &mut TaskState, reason: String) -> Vec<AgentEvent> {
        task.pending_approval = None;
        task.patch_text = None;
        task.patch_edits = None;
        task.patch_previewed = false;
        task.pending_agent_action = None;
        task.messages.push(TaskMessage {
            role: MessageRole::User,
            content: format!("reject: {reason}"),
            created_at: Utc::now(),
        });
        task.observations.push(task::Observation {
            id: format!("obs{}", task.observations.len() + 1),
            source: "user:reject".to_string(),
            kind: "user_rejection".to_string(),
            summary: reason,
            locations: Vec::new(),
            tokens: task::ObservationTokens { raw: 0, packet: 0 },
        });
        vec![AgentEvent::Progress("rejected pending patch; returning to planning".to_string())]
    }

    fn steer(&mut self, task: &mut TaskState, message: String) -> Vec<AgentEvent> {
        task.constraints.push(message.clone());
        task.messages.push(TaskMessage {
            role: MessageRole::User,
            content: format!("steer: {message}"),
            created_at: Utc::now(),
        });
        vec![AgentEvent::Progress(format!("steering recorded: {message}"))]
    }

    fn reply(&mut self, task: &mut TaskState, message: String) -> Vec<AgentEvent> {
        task.pending_interaction = None;
        task.pending_agent_action = None;
        task.messages.push(TaskMessage {
            role: MessageRole::User,
            content: message.clone(),
            created_at: Utc::now(),
        });
        task.observations.push(task::Observation {
            id: format!("obs{}", task.observations.len() + 1),
            source: "user:reply".to_string(),
            kind: "user_reply".to_string(),
            summary: message,
            locations: Vec::new(),
            tokens: task::ObservationTokens { raw: 0, packet: 0 },
        });
        vec![AgentEvent::Progress("reply recorded; resuming".to_string())]
    }

    fn add_context(
        &mut self,
        task: &mut TaskState,
        target: ContextTarget,
    ) -> io::Result<Vec<AgentEvent>> {
        task.pending_agent_action = Some(match target {
            ContextTarget::Symbol(target) => AgentAction::ReadSymbol { target },
            ContextTarget::Window { path, line } => AgentAction::ReadWindow { path, line, radius: 20 },
            ContextTarget::Search(query) => AgentAction::Search { query },
        });
        let step_index = task.route.len() + 1;
        let outcome = execute_step(&NodeOp::ReadContext, task, step_index)?;
        record_route(task, &NodeOp::ReadContext, &outcome);
        Ok(vec![AgentEvent::Progress(outcome.summary)])
    }

    fn verify(
        &mut self,
        task: &mut TaskState,
        command: VerificationCommand,
    ) -> io::Result<Vec<AgentEvent>> {
        task.pending_agent_action = Some(AgentAction::RunCommand {
            program: command.program.clone(),
            args: command.args.clone(),
        });
        let step_index = task.route.len() + 1;
        let outcome = execute_step(&NodeOp::ReadContext, task, step_index)?;
        record_route(task, &NodeOp::ReadContext, &outcome);

        // `verify <command>` both runs the check immediately and augments
        // the structured plan, so a later `RunFinalVerification` also
        // exercises it instead of only ever running it once, ad hoc.
        let plan = task.verification.get_or_insert_with(task::VerificationPlan::default);
        plan.checks.push(task::VerificationCheck {
            command,
            required: false,
            scope: task::VerificationScope::Targeted,
        });

        Ok(vec![AgentEvent::VerificationCompleted { summary: outcome.summary }])
    }

    fn stop(&mut self, task: &mut TaskState) -> AgentEvent {
        task.terminal_reason = Some(StopReason::Blocked);
        AgentEvent::Stopped(StopReason::Blocked)
    }
}

impl Default for AgentEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Turn a workflow `Stop` decision into the specific event the terminal
/// session or dashboard should render, based on what state caused it.
fn interpret_stop(task: &TaskState, reason: StopReason) -> AgentEvent {
    if reason == StopReason::Blocked {
        if let Some(interaction) = &task.pending_interaction {
            return AgentEvent::Question(interaction.clone());
        }
        if let Some(approval) = &task.pending_approval {
            return AgentEvent::ApprovalRequired(approval.clone());
        }
        if task
            .route
            .last()
            .map(|entry| entry.step == NodeOp::Report.name())
            .unwrap_or(false)
        {
            return AgentEvent::Finished(TaskOutcome {
                summary: task.patch_text.clone().unwrap_or_else(|| "task complete".to_string()),
                patch_applied: task.patch_applied,
            });
        }
    }
    AgentEvent::Stopped(reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent::workflow::Workflow;
    use crate::commands::task::{
        CurrentFailure, ProjectCard, RouteEntry, TaskBudget, TaskIntent, VerificationCheck,
        VerificationPlan, VerificationScope,
    };

    fn base_task() -> TaskState {
        TaskState {
            schema_version: 1,
            id: "task-engine-test".to_string(),
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
            pending_agent_action: None,
            intent: Some(TaskIntent::DebugFailure),
            current_failure: Some(CurrentFailure {
                kind: "test_failure".to_string(),
                summary: "assertion left 13 right 12".to_string(),
                locations: vec!["src/lib.rs:42".to_string()],
                detail: None,
            }),
            closed_at: None,
            project: Some(ProjectCard {
                language: "Rust".to_string(),
                test_command: "cargo test".to_string(),
                build_command: Some("cargo build".to_string()),
            }),
            verification: Some(VerificationPlan {
                checks: vec![VerificationCheck {
                    command: VerificationCommand {
                        program: "cargo".to_string(),
                        args: vec!["test".to_string()],
                    },
                    required: true,
                    scope: VerificationScope::FullProject,
                }],
                expected_baseline_exit: Some(101),
            }),
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
            pending_interaction: None,
            pending_approval: None,
            messages: Vec::new(),
            explicit_verify_commands: Vec::new(),
            inspected_digests: Default::default(),
            verification_results: Vec::new(),
        }
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

    #[test]
    fn ask_user_persists_pending_interaction_and_stops() {
        let mut task = base_task();
        push_route(&mut task, NodeOp::RunBaseline, "fail exit 101");
        push_route(&mut task, NodeOp::ExtractEvidence, "assertion left 13 right 12");
        push_route(&mut task, NodeOp::SelectContext, "no off-site symbols");
        push_route(&mut task, NodeOp::PlanContext, "ask user");
        task.pending_agent_action = Some(AgentAction::AskUser {
            question: "Which behaviour is correct?".to_string(),
        });

        let mut engine = AgentEngine::new();
        let events = engine.run_until_blocked(&mut task).unwrap();

        assert!(matches!(events.last(), Some(AgentEvent::Question(_))));
        assert_eq!(
            task.pending_interaction.as_ref().map(|i| i.question.clone()),
            Some("Which behaviour is correct?".to_string())
        );
    }

    #[test]
    fn reply_clears_pending_interaction_and_resumes() {
        let mut task = base_task();
        task.pending_interaction = Some(PendingInteraction {
            question: "Which behaviour is correct?".to_string(),
            asked_at: Utc::now(),
        });

        let mut engine = AgentEngine::new();
        let events = engine
            .advance(&mut task, ControlCommand::Reply { message: "the old one".to_string() })
            .unwrap();

        assert!(!events.is_empty());
        assert!(task.pending_interaction.is_none());
        assert!(task.observations.iter().any(|o| o.summary == "the old one"));
    }

    #[test]
    fn reject_records_reason_and_returns_to_planning() {
        let mut task = base_task();
        task.patch_text = Some("change expected value".to_string());
        task.pending_agent_action = Some(AgentAction::PlanPatch);

        let mut engine = AgentEngine::new();
        engine
            .advance(
                &mut task,
                ControlCommand::Reject { reason: "public API must remain unchanged".to_string() },
            )
            .unwrap();

        assert!(task.patch_text.is_none());
        assert!(task.pending_agent_action.is_none());
        assert!(
            task.observations
                .iter()
                .any(|o| o.summary == "public API must remain unchanged")
        );
    }

    #[test]
    fn steer_adds_durable_constraint() {
        let mut task = base_task();
        let mut engine = AgentEngine::new();
        engine
            .advance(
                &mut task,
                ControlCommand::Steer { message: "check overflow of the generation counter".to_string() },
            )
            .unwrap();

        assert!(
            task.constraints
                .iter()
                .any(|c| c == "check overflow of the generation counter")
        );
    }
}
