use std::{fmt, sync::OnceLock};

use serde::{Deserialize, Serialize};

use crate::{
    context::artifact::ContextRequirement,
    model::{ModelPurpose, ToolDefinition},
};

use super::ExecutorKind;
use super::workflow::NodeOp;

macro_rules! string_id_type {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

macro_rules! version_type {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(u16);

        impl $name {
            pub const V1: Self = Self(1);

            #[allow(dead_code)]
            pub fn new(value: u16) -> Self {
                Self(value)
            }

            pub fn get(self) -> u16 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "v{}", self.get())
            }
        }
    };
}

string_id_type!(PrimitiveId);
version_type!(PrimitiveVersion);
string_id_type!(PhaseId);
string_id_type!(ToolProfileId);
version_type!(ToolProfileVersion);
string_id_type!(PromptId);
version_type!(PromptVersion);
string_id_type!(OutputSchemaId);
version_type!(OutputSchemaVersion);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCategory {
    Goal,
    TaskGoal,
    ToolDefinitions,
    AcceptanceCriteria,
    Constraints,
    Budget,
    Intent,
    ProjectEnvironment,
    VerificationPlan,
    CurrentFailure,
    Observations,
    Hypotheses,
    AvailableContext,
    QueuedActions,
    PatchPlan,
    CurrentChanges,
    RouteHistory,
    FailureEvidence,
    RepositoryInventory,
    RelevantSymbol,
    RelevantWindow,
    CodeGraphCandidate,
    RecentToolOutput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectPolicy {
    ReadOnly,
    TaskStateMutation,
    ExternalCommand,
    WorkingTreeMutation,
    UserInteraction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryPolicy {
    SingleAttempt,
    BestEffort,
    WorkflowLoop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProfileCapability {
    PullAvailableContext,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ToolProfileCapabilities {
    pub pull_available_context: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolProfileDescriptor {
    pub id: ToolProfileId,
    pub version: ToolProfileVersion,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capability_flags: Vec<ToolProfileCapability>,
}

impl ToolProfileDescriptor {
    pub fn materialize(&self, capabilities: ToolProfileCapabilities) -> Vec<ToolDefinition> {
        match self.id.as_str() {
            "intent_classifier" => classifier_tools_v1(),
            "context_ranker" => context_ranker_tools_v1(),
            "context_planner" => context_planner_tools_v1(capabilities),
            "patch_editor" => patch_editor_tools_v1(),
            "no_tools" => Vec::new(),
            other => panic!("unknown tool profile `{other}`"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PromptDescriptor {
    pub id: PromptId,
    pub version: PromptVersion,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OutputSchemaRef {
    pub id: OutputSchemaId,
    pub version: OutputSchemaVersion,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PrimitiveSpec {
    pub id: PrimitiveId,
    pub version: PrimitiveVersion,
    pub phase: PhaseId,
    pub executor: ExecutorKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_context: Vec<ContextCategory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional_context: Vec<ContextCategory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_requirements: Vec<ContextRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_profile: Option<ToolProfileDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<PromptDescriptor>,
    pub output_schema: OutputSchemaRef,
    pub side_effect_policy: SideEffectPolicy,
    pub retry_policy: RetryPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PrimitiveRegistryEntry {
    pub op: NodeOp,
    pub spec: PrimitiveSpec,
}

#[derive(Clone, Debug)]
struct PromptCatalogEntry {
    purpose: ModelPurpose,
    descriptor: PromptDescriptor,
}

pub(crate) fn registry() -> &'static [PrimitiveRegistryEntry] {
    static REGISTRY: OnceLock<Vec<PrimitiveRegistryEntry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            vec![
                entry(
                    NodeOp::ClassifyIntent,
                    "classify_intent",
                    "intake",
                    NodeOp::ClassifyIntent.executor(),
                    vec![ContextCategory::TaskGoal],
                    vec![ContextCategory::Budget],
                    Some(intent_classifier_profile().clone()),
                    Some(prompt_for_purpose(ModelPurpose::IntentClassification).clone()),
                    output_schema("task_intent"),
                    SideEffectPolicy::TaskStateMutation,
                    RetryPolicy::WorkflowLoop,
                ),
                entry(
                    NodeOp::DetectProject,
                    "detect_project",
                    "investigation",
                    NodeOp::DetectProject.executor(),
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                    output_schema("project_card"),
                    SideEffectPolicy::TaskStateMutation,
                    RetryPolicy::SingleAttempt,
                ),
                entry(
                    NodeOp::ResolveVerification,
                    "resolve_verification",
                    "investigation",
                    NodeOp::ResolveVerification.executor(),
                    vec![ContextCategory::Intent, ContextCategory::ProjectEnvironment],
                    Vec::new(),
                    None,
                    None,
                    output_schema("verification_plan"),
                    SideEffectPolicy::TaskStateMutation,
                    RetryPolicy::SingleAttempt,
                ),
                entry(
                    NodeOp::RunBaseline,
                    "run_baseline",
                    "investigation",
                    NodeOp::RunBaseline.executor(),
                    vec![ContextCategory::VerificationPlan],
                    vec![ContextCategory::Intent],
                    None,
                    None,
                    output_schema("baseline_run_result"),
                    SideEffectPolicy::ExternalCommand,
                    RetryPolicy::WorkflowLoop,
                ),
                entry(
                    NodeOp::ExtractEvidence,
                    "extract_evidence",
                    "investigation",
                    NodeOp::ExtractEvidence.executor(),
                    Vec::new(),
                    vec![
                        ContextCategory::CurrentFailure,
                        ContextCategory::Observations,
                    ],
                    None,
                    None,
                    output_schema("evidence_summary"),
                    SideEffectPolicy::ReadOnly,
                    RetryPolicy::SingleAttempt,
                ),
                entry(
                    NodeOp::SelectContext,
                    "select_context",
                    "investigation",
                    NodeOp::SelectContext.executor(),
                    vec![
                        ContextCategory::CurrentFailure,
                        ContextCategory::CodeGraphCandidate,
                    ],
                    vec![ContextCategory::Budget],
                    Some(context_ranker_profile().clone()),
                    Some(prompt_for_purpose(ModelPurpose::ContextRanking).clone()),
                    output_schema("available_context"),
                    SideEffectPolicy::TaskStateMutation,
                    RetryPolicy::BestEffort,
                ),
                entry(
                    NodeOp::PlanContext,
                    "plan_context",
                    "planning",
                    NodeOp::PlanContext.executor(),
                    vec![ContextCategory::TaskGoal, ContextCategory::Budget],
                    vec![
                        ContextCategory::AcceptanceCriteria,
                        ContextCategory::Constraints,
                        ContextCategory::ProjectEnvironment,
                        ContextCategory::CurrentFailure,
                        ContextCategory::Observations,
                        ContextCategory::Hypotheses,
                        ContextCategory::AvailableContext,
                    ],
                    Some(context_planner_profile().clone()),
                    Some(prompt_for_purpose(ModelPurpose::AgentPlanner).clone()),
                    output_schema("planner_action"),
                    SideEffectPolicy::TaskStateMutation,
                    RetryPolicy::WorkflowLoop,
                ),
                entry(
                    NodeOp::ReadContext,
                    "read_context",
                    "investigation",
                    NodeOp::ReadContext.executor(),
                    vec![ContextCategory::QueuedActions],
                    vec![ContextCategory::AvailableContext],
                    None,
                    None,
                    output_schema("context_observation"),
                    SideEffectPolicy::TaskStateMutation,
                    RetryPolicy::SingleAttempt,
                ),
                entry(
                    NodeOp::PlanPatch,
                    "plan_patch",
                    "planning",
                    NodeOp::PlanPatch.executor(),
                    vec![ContextCategory::TaskGoal, ContextCategory::Observations],
                    vec![
                        ContextCategory::Intent,
                        ContextCategory::Budget,
                        ContextCategory::CurrentFailure,
                    ],
                    Some(patch_editor_profile().clone()),
                    Some(prompt_for_purpose(ModelPurpose::PatchGeneration).clone()),
                    output_schema("patch_edits"),
                    SideEffectPolicy::TaskStateMutation,
                    RetryPolicy::WorkflowLoop,
                ),
                entry(
                    NodeOp::ApplyPatch,
                    "apply_patch",
                    "implementation",
                    NodeOp::ApplyPatch.executor(),
                    vec![ContextCategory::PatchPlan],
                    vec![
                        ContextCategory::AvailableContext,
                        ContextCategory::Observations,
                    ],
                    None,
                    None,
                    output_schema("patch_apply_result"),
                    SideEffectPolicy::WorkingTreeMutation,
                    RetryPolicy::SingleAttempt,
                ),
                entry(
                    NodeOp::RunFinalVerification,
                    "run_final_verification",
                    "verification",
                    NodeOp::RunFinalVerification.executor(),
                    vec![ContextCategory::VerificationPlan],
                    vec![ContextCategory::PatchPlan],
                    None,
                    None,
                    output_schema("final_verification_result"),
                    SideEffectPolicy::ExternalCommand,
                    RetryPolicy::WorkflowLoop,
                ),
                entry(
                    NodeOp::RetryFix,
                    "retry_fix",
                    "verification",
                    NodeOp::RetryFix.executor(),
                    Vec::new(),
                    vec![
                        ContextCategory::CurrentFailure,
                        ContextCategory::PatchPlan,
                        ContextCategory::QueuedActions,
                        ContextCategory::RouteHistory,
                    ],
                    None,
                    None,
                    output_schema("retry_state"),
                    SideEffectPolicy::TaskStateMutation,
                    RetryPolicy::WorkflowLoop,
                ),
                entry(
                    NodeOp::AskUser,
                    "ask_user",
                    "interaction",
                    NodeOp::AskUser.executor(),
                    Vec::new(),
                    vec![ContextCategory::Budget, ContextCategory::Observations],
                    None,
                    None,
                    output_schema("user_question"),
                    SideEffectPolicy::UserInteraction,
                    RetryPolicy::SingleAttempt,
                ),
                entry(
                    NodeOp::DirectAnswer,
                    "direct_answer",
                    "reporting",
                    NodeOp::DirectAnswer.executor(),
                    vec![ContextCategory::TaskGoal],
                    vec![ContextCategory::Observations, ContextCategory::Budget],
                    Some(no_tools_profile().clone()),
                    Some(prompt_for_purpose(ModelPurpose::FinalReport).clone()),
                    output_schema("direct_answer"),
                    SideEffectPolicy::TaskStateMutation,
                    RetryPolicy::WorkflowLoop,
                ),
                entry(
                    NodeOp::Report,
                    "report",
                    "reporting",
                    NodeOp::Report.executor(),
                    Vec::new(),
                    vec![ContextCategory::PatchPlan, ContextCategory::RouteHistory],
                    None,
                    None,
                    output_schema("task_report"),
                    SideEffectPolicy::ReadOnly,
                    RetryPolicy::SingleAttempt,
                ),
            ]
        })
        .as_slice()
}

pub(crate) fn primitive_for_node_op(op: &NodeOp) -> Option<&'static PrimitiveSpec> {
    registry()
        .iter()
        .find(|entry| entry.op == *op)
        .map(|entry| &entry.spec)
}

#[allow(dead_code)]
pub(crate) fn node_op_for_primitive(
    primitive_id: &PrimitiveId,
    version: PrimitiveVersion,
) -> Option<NodeOp> {
    registry()
        .iter()
        .find(|entry| entry.spec.id == *primitive_id && entry.spec.version == version)
        .map(|entry| entry.op)
}

#[allow(dead_code)]
pub(crate) fn primitive_by_stable_id(primitive_id: &PrimitiveId) -> Option<&'static PrimitiveSpec> {
    registry()
        .iter()
        .find(|entry| entry.spec.id == *primitive_id)
        .map(|entry| &entry.spec)
}

pub(crate) fn tool_profiles() -> &'static [ToolProfileDescriptor] {
    static TOOL_PROFILES: OnceLock<Vec<ToolProfileDescriptor>> = OnceLock::new();
    TOOL_PROFILES
        .get_or_init(|| {
            vec![
                ToolProfileDescriptor {
                    id: ToolProfileId::new("intent_classifier"),
                    version: ToolProfileVersion::V1,
                    capability_flags: Vec::new(),
                },
                ToolProfileDescriptor {
                    id: ToolProfileId::new("context_ranker"),
                    version: ToolProfileVersion::V1,
                    capability_flags: Vec::new(),
                },
                ToolProfileDescriptor {
                    id: ToolProfileId::new("context_planner"),
                    version: ToolProfileVersion::V1,
                    capability_flags: vec![ToolProfileCapability::PullAvailableContext],
                },
                ToolProfileDescriptor {
                    id: ToolProfileId::new("patch_editor"),
                    version: ToolProfileVersion::V1,
                    capability_flags: Vec::new(),
                },
                ToolProfileDescriptor {
                    id: ToolProfileId::new("no_tools"),
                    version: ToolProfileVersion::V1,
                    capability_flags: Vec::new(),
                },
            ]
        })
        .as_slice()
}

pub(crate) fn prompt_for_purpose(purpose: ModelPurpose) -> &'static PromptDescriptor {
    static PROMPTS: OnceLock<Vec<PromptCatalogEntry>> = OnceLock::new();
    PROMPTS
        .get_or_init(|| {
            vec![
                PromptCatalogEntry {
                    purpose: ModelPurpose::IntentClassification,
                    descriptor: PromptDescriptor {
                        id: PromptId::new("intent_classification"),
                        version: PromptVersion::V1,
                    },
                },
                PromptCatalogEntry {
                    purpose: ModelPurpose::ContextRanking,
                    descriptor: PromptDescriptor {
                        id: PromptId::new("context_ranking"),
                        version: PromptVersion::V1,
                    },
                },
                PromptCatalogEntry {
                    purpose: ModelPurpose::AgentPlanner,
                    descriptor: PromptDescriptor {
                        id: PromptId::new("context_planner"),
                        version: PromptVersion::V1,
                    },
                },
                PromptCatalogEntry {
                    purpose: ModelPurpose::PatchGeneration,
                    descriptor: PromptDescriptor {
                        id: PromptId::new("patch_generation"),
                        version: PromptVersion::V1,
                    },
                },
                PromptCatalogEntry {
                    purpose: ModelPurpose::FinalReport,
                    descriptor: PromptDescriptor {
                        id: PromptId::new("direct_answer"),
                        version: PromptVersion::V1,
                    },
                },
            ]
        })
        .iter()
        .find(|entry| entry.purpose == purpose)
        .map(|entry| &entry.descriptor)
        .unwrap_or_else(|| panic!("no prompt descriptor for model purpose `{purpose}`"))
}

#[allow(dead_code)]
pub(crate) fn prompt_descriptors() -> Vec<&'static PromptDescriptor> {
    vec![
        prompt_for_purpose(ModelPurpose::IntentClassification),
        prompt_for_purpose(ModelPurpose::ContextRanking),
        prompt_for_purpose(ModelPurpose::AgentPlanner),
        prompt_for_purpose(ModelPurpose::PatchGeneration),
        prompt_for_purpose(ModelPurpose::FinalReport),
    ]
}

pub(crate) fn intent_classifier_profile() -> &'static ToolProfileDescriptor {
    tool_profile("intent_classifier")
}

pub(crate) fn context_ranker_profile() -> &'static ToolProfileDescriptor {
    tool_profile("context_ranker")
}

pub(crate) fn context_planner_profile() -> &'static ToolProfileDescriptor {
    tool_profile("context_planner")
}

pub(crate) fn patch_editor_profile() -> &'static ToolProfileDescriptor {
    tool_profile("patch_editor")
}

pub(crate) fn no_tools_profile() -> &'static ToolProfileDescriptor {
    tool_profile("no_tools")
}

fn tool_profile(id: &str) -> &'static ToolProfileDescriptor {
    tool_profiles()
        .iter()
        .find(|profile| profile.id.as_str() == id)
        .unwrap_or_else(|| panic!("missing tool profile `{id}`"))
}

#[allow(clippy::too_many_arguments)]
fn entry(
    op: NodeOp,
    primitive_id: &str,
    phase: &str,
    executor: ExecutorKind,
    required_context: Vec<ContextCategory>,
    optional_context: Vec<ContextCategory>,
    tool_profile: Option<ToolProfileDescriptor>,
    prompt: Option<PromptDescriptor>,
    output_schema: OutputSchemaRef,
    side_effect_policy: SideEffectPolicy,
    retry_policy: RetryPolicy,
) -> PrimitiveRegistryEntry {
    let context_requirements = required_context
        .iter()
        .enumerate()
        .map(|(index, category)| ContextRequirement::required(*category, index as u16))
        .chain(
            optional_context
                .iter()
                .enumerate()
                .map(|(index, category)| {
                    ContextRequirement::optional(*category, (required_context.len() + index) as u16)
                }),
        )
        .collect();
    PrimitiveRegistryEntry {
        op,
        spec: PrimitiveSpec {
            id: PrimitiveId::new(primitive_id),
            version: PrimitiveVersion::V1,
            phase: PhaseId::new(phase),
            executor,
            required_context,
            optional_context,
            context_requirements,
            tool_profile,
            prompt,
            output_schema,
            side_effect_policy,
            retry_policy,
        },
    }
}

fn output_schema(id: &str) -> OutputSchemaRef {
    OutputSchemaRef {
        id: OutputSchemaId::new(id),
        version: OutputSchemaVersion::V1,
    }
}

fn classifier_tools_v1() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "classify",
        description: "Record the best-fitting intent.",
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

fn context_ranker_tools_v1() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "judge_relevance",
        description: "Record the ids of candidates whose definition is relevant to fixing the failure at its source (the candidate is already known to be on the call path from the failure). Judge from the observed assertion diff and each candidate's actual logic, not from test/symbol names or comments, which are unverified labels.",
        parameters: serde_json::json!({
            "type": "object",
            "required": ["relevant_ids"],
            "additionalProperties": false,
            "properties": {
                "relevant_ids": { "type": "array", "items": { "type": "string" } }
            }
        }),
    }]
}

fn context_planner_tools_v1(capabilities: ToolProfileCapabilities) -> Vec<ToolDefinition> {
    let mut tools = vec![
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
    ];

    if capabilities.pull_available_context {
        tools.push(ToolDefinition {
            name: "pull",
            description: "Load the body of an available off-site symbol into context.",
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "additionalProperties": false,
                "properties": {
                    "id": { "type": "string" }
                }
            }),
        });
    }

    tools
}

fn patch_editor_tools_v1() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "propose_edits",
        description: "Minimal exact string edits that fix the failure. Each `find` must appear verbatim, and uniquely, in `path`.",
        parameters: serde_json::json!({
            "type": "object",
            "required": ["edits"],
            "additionalProperties": false,
            "properties": {
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["path", "find", "replace"],
                        "additionalProperties": false,
                        "properties": {
                            "path":    { "type": "string" },
                            "find":    { "type": "string" },
                            "replace": { "type": "string" }
                        }
                    }
                }
            }
        }),
    }]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn identity(id: &str, version: u16) -> String {
        format!("{id}@v{version}")
    }

    fn planner_tools_expected(with_pull: bool) -> Vec<ToolDefinition> {
        let mut tools = vec![
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
        ];
        if with_pull {
            tools.push(ToolDefinition {
                name: "pull",
                description: "Load the body of an available off-site symbol into context.",
                parameters: serde_json::json!({
                    "type": "object",
                    "required": ["id"],
                    "additionalProperties": false,
                    "properties": {
                        "id": { "type": "string" }
                    }
                }),
            });
        }
        tools
    }

    #[test]
    fn every_node_op_has_exactly_one_primitive() {
        for op in NodeOp::all() {
            let primitive = primitive_for_node_op(op).expect("missing primitive mapping");
            assert_eq!(
                node_op_for_primitive(&primitive.id, primitive.version),
                Some(*op),
                "reverse lookup mismatch for {:?}",
                op
            );
            assert_eq!(
                primitive_by_stable_id(&primitive.id)
                    .expect("stable id lookup should resolve")
                    .id,
                primitive.id
            );
        }
    }

    #[test]
    fn primitive_identities_are_unique() {
        let mut seen = BTreeSet::new();
        for entry in registry() {
            let inserted = seen.insert(identity(entry.spec.id.as_str(), entry.spec.version.get()));
            assert!(inserted, "duplicate primitive identity for {:?}", entry.op);
        }
    }

    #[test]
    fn prompt_tool_profile_and_output_schema_identities_are_unique() {
        let prompt_ids: BTreeSet<_> = prompt_descriptors()
            .into_iter()
            .map(|descriptor| identity(descriptor.id.as_str(), descriptor.version.get()))
            .collect();
        assert_eq!(prompt_ids.len(), prompt_descriptors().len());

        let tool_profile_ids: BTreeSet<_> = tool_profiles()
            .iter()
            .map(|descriptor| identity(descriptor.id.as_str(), descriptor.version.get()))
            .collect();
        assert_eq!(tool_profile_ids.len(), tool_profiles().len());

        let output_schema_ids: BTreeSet<_> = registry()
            .iter()
            .map(|entry| {
                identity(
                    entry.spec.output_schema.id.as_str(),
                    entry.spec.output_schema.version.get(),
                )
            })
            .collect();
        assert_eq!(output_schema_ids.len(), registry().len());
    }

    #[test]
    fn registry_executor_matches_node_op_executor() {
        for entry in registry() {
            assert_eq!(entry.spec.executor, entry.op.executor(), "{:?}", entry.op);
        }
    }

    #[test]
    fn intent_classifier_profile_matches_legacy_tool_definition() {
        let actual = serde_json::to_string(
            &intent_classifier_profile().materialize(ToolProfileCapabilities::default()),
        )
        .unwrap();
        let expected = serde_json::to_string(&classifier_tools_v1()).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn context_ranker_profile_matches_legacy_tool_definition() {
        let actual = serde_json::to_string(
            &context_ranker_profile().materialize(ToolProfileCapabilities::default()),
        )
        .unwrap();
        let expected = serde_json::to_string(&context_ranker_tools_v1()).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn context_planner_profile_matches_legacy_tool_definitions() {
        let without_pull = serde_json::to_string(
            &context_planner_profile().materialize(ToolProfileCapabilities::default()),
        )
        .unwrap();
        let expected_without_pull = serde_json::to_string(&planner_tools_expected(false)).unwrap();
        assert_eq!(without_pull, expected_without_pull);

        let with_pull = serde_json::to_string(&context_planner_profile().materialize(
            ToolProfileCapabilities {
                pull_available_context: true,
            },
        ))
        .unwrap();
        let expected_with_pull = serde_json::to_string(&planner_tools_expected(true)).unwrap();
        assert_eq!(with_pull, expected_with_pull);
    }

    #[test]
    fn patch_editor_profile_matches_legacy_tool_definition() {
        let actual = serde_json::to_string(
            &patch_editor_profile().materialize(ToolProfileCapabilities::default()),
        )
        .unwrap();
        let expected = serde_json::to_string(&patch_editor_tools_v1()).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn no_tools_profile_is_empty() {
        assert!(
            no_tools_profile()
                .materialize(ToolProfileCapabilities::default())
                .is_empty()
        );
    }
}
