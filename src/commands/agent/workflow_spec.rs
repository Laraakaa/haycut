use std::io;

use serde::{Deserialize, Serialize};

use crate::commands::task::{TaskIntent, TaskState};

use super::{
    primitive,
    workflow::{self, Node, NodeOp, NodeStatus, Workflow},
};

const WORKFLOW_SPEC_SCHEMA_VERSION: u8 = 1;
const COMPATIBILITY_COMPILER_VERSION: &str = "phase1_compat_v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowGuard {
    RequiresRepository,
    RequiresVerification,
    ExpectsBaselineFailure,
    PatchExpected,
    RequiresFinalVerification,
    AnswersWithoutPatch,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowNodeSpec {
    pub id: String,
    pub primitive_id: primitive::PrimitiveId,
    pub primitive_version: primitive::PrimitiveVersion,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<WorkflowGuard>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSpec {
    pub schema_version: u8,
    pub compiler_version: String,
    pub entrypoints: Vec<String>,
    pub nodes: Vec<WorkflowNodeSpec>,
}

impl WorkflowSpec {
    #[allow(dead_code)]
    pub fn to_workflow(&self) -> io::Result<Workflow> {
        let seq = self
            .nodes
            .iter()
            .filter_map(|node| node.id.strip_prefix('n'))
            .filter_map(|suffix| suffix.parse::<u32>().ok())
            .max()
            .unwrap_or(0);

        let mut nodes = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let op = primitive::node_op_for_primitive(&node.primitive_id, node.primitive_version)
                .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "unknown primitive {}@{} in workflow spec node {}",
                        node.primitive_id, node.primitive_version, node.id
                    ),
                )
            })?;
            nodes.push(Node {
                id: node.id.clone(),
                op,
                depends_on: node.dependencies.clone(),
                status: NodeStatus::Pending,
                produced_by: node.dependencies.last().cloned(),
                outcome: None,
            });
        }

        Ok(Workflow { nodes, seq })
    }
}

pub(crate) fn compile_compatibility_spec(task: &TaskState) -> WorkflowSpec {
    let ops = compatibility_route(task.intent);
    let nodes = ops
        .into_iter()
        .enumerate()
        .map(|(index, op)| {
            let primitive = primitive::primitive_for_node_op(&op)
                .expect("every node op should have a registered primitive");
            let id = format!("n{}", index + 1);
            WorkflowNodeSpec {
                id: id.clone(),
                primitive_id: primitive.id.clone(),
                primitive_version: primitive.version,
                dependencies: if index > 0 {
                    vec![format!("n{}", index)]
                } else {
                    Vec::new()
                },
                guard: guard_for(op),
            }
        })
        .collect();

    WorkflowSpec {
        schema_version: WORKFLOW_SPEC_SCHEMA_VERSION,
        compiler_version: COMPATIBILITY_COMPILER_VERSION.to_string(),
        entrypoints: vec!["n1".to_string()],
        nodes,
    }
}

fn compatibility_route(intent: Option<TaskIntent>) -> Vec<NodeOp> {
    let mut ops = vec![NodeOp::ClassifyIntent];
    let Some(intent) = intent else {
        return ops;
    };

    let policy = workflow::IntentPolicy::for_intent(intent);

    if policy.needs_repo {
        ops.push(NodeOp::DetectProject);
    }

    if policy.requires_verification {
        ops.push(NodeOp::ResolveVerification);
        ops.push(NodeOp::RunBaseline);
    }

    if policy.expect_baseline_failure {
        ops.push(NodeOp::ExtractEvidence);
        if policy.patch_expected {
            ops.push(NodeOp::SelectContext);
        }
    }

    if policy.patch_expected {
        ops.push(NodeOp::PlanContext);
        ops.push(NodeOp::ReadContext);
        ops.push(NodeOp::PlanPatch);
        ops.push(NodeOp::ApplyPatch);
        if policy.require_final_verification {
            ops.push(NodeOp::RunFinalVerification);
        }
    } else {
        ops.push(NodeOp::DirectAnswer);
    }

    ops.push(NodeOp::Report);
    ops
}

fn guard_for(op: NodeOp) -> Option<WorkflowGuard> {
    match op {
        NodeOp::DetectProject => Some(WorkflowGuard::RequiresRepository),
        NodeOp::ResolveVerification | NodeOp::RunBaseline => {
            Some(WorkflowGuard::RequiresVerification)
        }
        NodeOp::ExtractEvidence | NodeOp::SelectContext => {
            Some(WorkflowGuard::ExpectsBaselineFailure)
        }
        NodeOp::PlanContext | NodeOp::ReadContext | NodeOp::PlanPatch | NodeOp::ApplyPatch => {
            Some(WorkflowGuard::PatchExpected)
        }
        NodeOp::RunFinalVerification => Some(WorkflowGuard::RequiresFinalVerification),
        NodeOp::DirectAnswer => Some(WorkflowGuard::AnswersWithoutPatch),
        NodeOp::ClassifyIntent | NodeOp::RetryFix | NodeOp::AskUser | NodeOp::Report => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::agent::AgentAction;
    use crate::commands::task::{
        CurrentFailure, ProjectCard, RouteEntry, TaskBudget, VerificationCheck,
        VerificationCommand, VerificationPlan, VerificationScope,
    };

    use super::*;

    fn empty_task() -> TaskState {
        TaskState {
            schema_version: 1,
            id: "task-test".to_string(),
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
            pending_interaction: None,
            pending_approval: None,
            messages: Vec::new(),
            explicit_verify_commands: Vec::new(),
            inspected_digests: Default::default(),
            verification_results: Vec::new(),
        }
    }

    fn task_with_intent(intent: TaskIntent) -> TaskState {
        let mut task = empty_task();
        task.intent = Some(intent);
        task
    }

    fn spec_ops(spec: &WorkflowSpec) -> Vec<NodeOp> {
        spec.nodes
            .iter()
            .map(|node| {
                primitive::node_op_for_primitive(&node.primitive_id, node.primitive_version)
                    .expect("workflow spec node should map back to a node op")
            })
            .collect()
    }

    fn route_entry(step: NodeOp, outcome: &str) -> RouteEntry {
        let primitive = primitive::primitive_for_node_op(&step)
            .expect("every simulated node op should have a primitive");
        RouteEntry {
            step: step.name().to_string(),
            executor: step.executor(),
            primitive_id: Some(primitive.id.clone()),
            primitive_version: Some(primitive.version),
            outcome: outcome.to_string(),
        }
    }

    fn simulate_success_path(intent: TaskIntent) -> Vec<NodeOp> {
        let mut task = empty_task();
        let mut workflow = task.workflow.clone();
        let mut ops = Vec::new();

        loop {
            let decision = workflow::next_ready_node(&mut workflow, &task, 2);
            let workflow::Decision::Ready(node_id, op) = decision else {
                break;
            };
            ops.push(op);
            workflow.complete(node_id, op, "ok".to_string());
            task.route.push(route_entry(op, outcome_for(op)));
            apply_success_state(&mut task, intent, op);
        }

        ops
    }

    fn outcome_for(op: NodeOp) -> &'static str {
        match op {
            NodeOp::RunBaseline => "baseline `cargo test` exited 0",
            NodeOp::RunFinalVerification => "final verification `cargo test` exited 0 (pass)",
            _ => "ok",
        }
    }

    fn apply_success_state(task: &mut TaskState, intent: TaskIntent, op: NodeOp) {
        match op {
            NodeOp::ClassifyIntent => task.intent = Some(intent),
            NodeOp::DetectProject => {
                task.project = Some(ProjectCard {
                    language: "Rust".to_string(),
                    test_command: "cargo test".to_string(),
                    build_command: Some("cargo build".to_string()),
                });
            }
            NodeOp::ResolveVerification => {
                task.verification = Some(VerificationPlan {
                    checks: vec![VerificationCheck {
                        command: VerificationCommand {
                            program: "cargo".to_string(),
                            args: vec!["test".to_string()],
                        },
                        required: true,
                        scope: VerificationScope::FullProject,
                    }],
                    expected_baseline_exit: (intent == TaskIntent::DebugFailure).then_some(101),
                });
            }
            NodeOp::ExtractEvidence => {
                task.current_failure = Some(CurrentFailure {
                    kind: "test_failure".to_string(),
                    summary: "assertion left 13 right 12".to_string(),
                    locations: vec!["src/lib.rs:42".to_string()],
                    detail: None,
                });
            }
            NodeOp::PlanContext => {
                task.pending_agent_action = Some(AgentAction::ReadWindow {
                    path: "src/lib.rs".into(),
                    line: 42,
                    radius: 20,
                });
            }
            NodeOp::ReadContext => {
                // The single-pass compatibility route only lists one
                // PlanContext/ReadContext pair, so the simulated planner
                // decides to patch on this round trip; the real, iterative
                // loop (see workflow.rs tests) can repeat this pair.
                task.pending_agent_action = Some(AgentAction::PlanPatch);
            }
            NodeOp::PlanPatch => {
                task.patch_text = Some("change expected value".to_string());
            }
            NodeOp::ApplyPatch => {
                task.patch_applied = true;
            }
            NodeOp::RunBaseline
            | NodeOp::SelectContext
            | NodeOp::RunFinalVerification
            | NodeOp::RetryFix
            | NodeOp::AskUser
            | NodeOp::DirectAnswer
            | NodeOp::Report => {}
        }
    }

    #[test]
    fn compiles_expected_routes_for_each_intent() {
        let debug = spec_ops(&compile_compatibility_spec(&task_with_intent(
            TaskIntent::DebugFailure,
        )));
        assert_eq!(
            debug,
            vec![
                NodeOp::ClassifyIntent,
                NodeOp::DetectProject,
                NodeOp::ResolveVerification,
                NodeOp::RunBaseline,
                NodeOp::ExtractEvidence,
                NodeOp::SelectContext,
                NodeOp::PlanContext,
                NodeOp::ReadContext,
                NodeOp::PlanPatch,
                NodeOp::ApplyPatch,
                NodeOp::RunFinalVerification,
                NodeOp::Report,
            ]
        );

        let implement = spec_ops(&compile_compatibility_spec(&task_with_intent(
            TaskIntent::ImplementFeature,
        )));
        assert_eq!(
            implement,
            vec![
                NodeOp::ClassifyIntent,
                NodeOp::DetectProject,
                NodeOp::ResolveVerification,
                NodeOp::RunBaseline,
                NodeOp::PlanContext,
                NodeOp::ReadContext,
                NodeOp::PlanPatch,
                NodeOp::ApplyPatch,
                NodeOp::RunFinalVerification,
                NodeOp::Report,
            ]
        );

        let refactor = spec_ops(&compile_compatibility_spec(&task_with_intent(
            TaskIntent::Refactor,
        )));
        assert_eq!(refactor, implement);

        let answer = spec_ops(&compile_compatibility_spec(&task_with_intent(
            TaskIntent::AnswerQuestion,
        )));
        assert_eq!(
            answer,
            vec![
                NodeOp::ClassifyIntent,
                NodeOp::DetectProject,
                NodeOp::DirectAnswer,
                NodeOp::Report,
            ]
        );
    }

    #[test]
    fn compiled_routes_match_current_workflow_success_paths() {
        for intent in [
            TaskIntent::DebugFailure,
            TaskIntent::ImplementFeature,
            TaskIntent::Refactor,
            TaskIntent::AnswerQuestion,
        ] {
            let expected = spec_ops(&compile_compatibility_spec(&task_with_intent(intent)));
            let simulated = simulate_success_path(intent);
            assert_eq!(simulated, expected, "{intent:?}");
        }
    }

    #[test]
    fn workflow_spec_adapter_preserves_ids_and_dependencies() {
        let spec = compile_compatibility_spec(&task_with_intent(TaskIntent::DebugFailure));
        let workflow = spec.to_workflow().expect("spec should adapt");

        assert_eq!(workflow.seq, spec.nodes.len() as u32);
        for (node, spec_node) in workflow.nodes.iter().zip(spec.nodes.iter()) {
            assert_eq!(node.id, spec_node.id);
            assert_eq!(node.depends_on, spec_node.dependencies);
            assert_eq!(node.status, NodeStatus::Pending);
            assert_eq!(node.produced_by, spec_node.dependencies.last().cloned());
        }
    }

    #[test]
    fn unknown_intent_compiles_to_seed_workflow_only() {
        let spec = compile_compatibility_spec(&empty_task());
        assert_eq!(spec.entrypoints, vec!["n1".to_string()]);
        assert_eq!(spec_ops(&spec), vec![NodeOp::ClassifyIntent]);
    }

    #[test]
    fn old_task_json_loads_without_workflow_spec() {
        let task = task_with_intent(TaskIntent::DebugFailure);
        let mut value = serde_json::to_value(&task).expect("task should serialize");
        value
            .as_object_mut()
            .expect("task should be an object")
            .remove("workflow_spec");

        let loaded: TaskState = serde_json::from_value(value).expect("old task should load");

        assert!(loaded.workflow_spec.is_none());
    }

    #[test]
    fn task_json_round_trips_workflow_spec() {
        let mut task = task_with_intent(TaskIntent::DebugFailure);
        task.workflow_spec = Some(compile_compatibility_spec(&task));

        let encoded = serde_json::to_string(&task).expect("task should serialize");
        let loaded: TaskState = serde_json::from_str(&encoded).expect("task should load");

        assert_eq!(loaded.workflow_spec, task.workflow_spec);
    }
}
