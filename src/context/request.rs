use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    commands::agent::primitive::{
        ContextCategory, PhaseId, PrimitiveId, PrimitiveSpec, PrimitiveVersion, PromptDescriptor,
        ToolProfileDescriptor,
    },
    model::{
        EstimatedTokenUsage, ModelPurpose, ModelRequest, ToolDefinition, estimate_tool_tokens,
    },
    util::estimate_tokens,
};

use super::digest::{DIGEST_SCHEMA_VERSION, digest_bytes, digest_json};

pub const REQUEST_MANIFEST_SCHEMA_VERSION: u16 = 1;
pub const CONTEXT_SEGMENT_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRole {
    System,
    ToolDefinition,
    Instruction,
    Repository,
    Task,
    Checkpoint,
    Context,
    Evidence,
    RecentOutput,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRepresentation {
    Raw,
    Extracted,
    Compressed,
    Generated,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePolicy {
    NoStore,
    Request,
    Reusable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextSegment {
    pub id: String,
    pub position: usize,
    pub role: ContextRole,
    pub category: ContextCategory,
    pub representation: ContextRepresentation,
    pub schema_version: u16,
    pub producer_id: String,
    pub producer_version: u16,
    pub content: String,
    pub content_digest: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provenance: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependency_digests: Vec<String>,
    pub byte_size: usize,
    pub estimated_tokens: usize,
    pub cache_policy: CachePolicy,
}

impl ContextSegment {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        position: usize,
        role: ContextRole,
        category: ContextCategory,
        representation: ContextRepresentation,
        producer_id: impl Into<String>,
        producer_version: u16,
        content: impl Into<String>,
        cache_policy: CachePolicy,
    ) -> Self {
        let content = content.into();
        Self {
            id: id.into(),
            position,
            role,
            category,
            representation,
            schema_version: CONTEXT_SEGMENT_SCHEMA_VERSION,
            producer_id: producer_id.into(),
            producer_version,
            content_digest: digest_bytes(
                "context-segment-content",
                CONTEXT_SEGMENT_SCHEMA_VERSION,
                content.as_bytes(),
            ),
            byte_size: content.len(),
            estimated_tokens: estimate_tokens(content.as_bytes()),
            content,
            provenance: BTreeMap::new(),
            dependency_digests: Vec::new(),
            cache_policy,
        }
    }

    pub fn descriptor(&self) -> RequestSegmentDescriptor {
        RequestSegmentDescriptor {
            id: self.id.clone(),
            position: self.position,
            role: self.role,
            category: self.category,
            representation: self.representation,
            schema_version: self.schema_version,
            producer_id: self.producer_id.clone(),
            producer_version: self.producer_version,
            content_digest: self.content_digest.clone(),
            provenance: self.provenance.clone(),
            dependency_digests: self.dependency_digests.clone(),
            byte_size: self.byte_size,
            estimated_tokens: self.estimated_tokens,
            cache_policy: self.cache_policy,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestSegmentDescriptor {
    pub id: String,
    pub position: usize,
    pub role: ContextRole,
    pub category: ContextCategory,
    pub representation: ContextRepresentation,
    pub schema_version: u16,
    pub producer_id: String,
    pub producer_version: u16,
    pub content_digest: String,
    pub provenance: BTreeMap<String, String>,
    pub dependency_digests: Vec<String>,
    pub byte_size: usize,
    pub estimated_tokens: usize,
    pub cache_policy: CachePolicy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestStatus {
    Prepared,
    Completed,
    ProviderFailed,
    RecordingFailed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestCorrelation {
    pub task_id: String,
    pub step_index: usize,
    pub node_id: Option<String>,
    pub workflow_compiler_version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestManifestDraft {
    pub schema_version: u16,
    pub id: String,
    pub task_id: String,
    pub step_index: usize,
    pub node_id: Option<String>,
    pub workflow_compiler_version: Option<String>,
    pub primitive_id: PrimitiveId,
    pub primitive_version: PrimitiveVersion,
    pub phase: PhaseId,
    pub purpose: ModelPurpose,
    pub prompt: Option<PromptDescriptor>,
    pub tool_profile: Option<ToolProfileDescriptor>,
    pub reasoning_effort: Option<String>,
    pub segments: Vec<RequestSegmentDescriptor>,
    pub request_digest: String,
    pub status: ManifestStatus,
    pub estimated_usage: EstimatedTokenUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison_json: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssembledRequest {
    pub request: ModelRequest,
    pub manifest: RequestManifestDraft,
}

pub struct RequestAssembly<'a> {
    pub primitive: &'a PrimitiveSpec,
    pub system_segments: Vec<ContextSegment>,
    pub user_segments: Vec<ContextSegment>,
    pub tools: &'a [ToolDefinition],
    pub purpose: ModelPurpose,
    pub max_output_tokens: usize,
    pub reasoning_effort: Option<String>,
    pub correlation: RequestCorrelation,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct RequestDigestInput<'a> {
    purpose: ModelPurpose,
    system: &'a Option<String>,
    prompt: &'a str,
    tools: &'a [ToolDefinition],
    max_output_tokens: usize,
    reasoning_effort: &'a Option<String>,
    metadata: &'a BTreeMap<String, String>,
}

pub fn assemble(input: RequestAssembly<'_>) -> Result<AssembledRequest, serde_json::Error> {
    let system = (!input.system_segments.is_empty()).then(|| {
        input
            .system_segments
            .iter()
            .map(|segment| segment.content.as_str())
            .collect::<String>()
    });
    let prompt = input
        .user_segments
        .iter()
        .map(|segment| segment.content.as_str())
        .collect::<String>();
    let estimated_usage = EstimatedTokenUsage {
        input: system
            .as_ref()
            .map(|value| estimate_tokens(value.as_bytes()))
            .unwrap_or(0)
            + estimate_tokens(prompt.as_bytes())
            + estimate_tool_tokens(input.tools),
        output: input.max_output_tokens,
    };
    let request_digest = digest_json(
        "model-request",
        DIGEST_SCHEMA_VERSION,
        &RequestDigestInput {
            purpose: input.purpose,
            system: &system,
            prompt: &prompt,
            tools: input.tools,
            max_output_tokens: input.max_output_tokens,
            reasoning_effort: &input.reasoning_effort,
            metadata: &input.metadata,
        },
    )?;
    let manifest_id = format!("request-manifest-{}", uuid::Uuid::new_v4().simple());
    let mut segments = input.system_segments;
    let tool_producer_id = input
        .primitive
        .tool_profile
        .as_ref()
        .map(|profile| profile.id.as_str())
        .unwrap_or_else(|| input.primitive.id.as_str());
    let tool_producer_version = input
        .primitive
        .tool_profile
        .as_ref()
        .map(|profile| profile.version.get())
        .unwrap_or_else(|| input.primitive.version.get());
    for (index, tool) in input.tools.iter().enumerate() {
        segments.push(ContextSegment::new(
            format!("tool-{index}"),
            0,
            ContextRole::ToolDefinition,
            ContextCategory::ToolDefinitions,
            ContextRepresentation::Generated,
            tool_producer_id,
            tool_producer_version,
            serde_json::to_string(tool)?,
            CachePolicy::Request,
        ));
    }
    segments.extend(input.user_segments);
    for (position, segment) in segments.iter_mut().enumerate() {
        segment.position = position;
    }

    Ok(AssembledRequest {
        request: ModelRequest {
            purpose: input.purpose,
            system,
            prompt,
            estimated_tokens: estimated_usage,
            max_output_tokens: Some(input.max_output_tokens),
            reasoning_effort: input.reasoning_effort.clone(),
            metadata: input.metadata,
        },
        manifest: RequestManifestDraft {
            schema_version: REQUEST_MANIFEST_SCHEMA_VERSION,
            id: manifest_id,
            task_id: input.correlation.task_id,
            step_index: input.correlation.step_index,
            node_id: input.correlation.node_id,
            workflow_compiler_version: input.correlation.workflow_compiler_version,
            primitive_id: input.primitive.id.clone(),
            primitive_version: input.primitive.version,
            phase: input.primitive.phase.clone(),
            purpose: input.purpose,
            prompt: input.primitive.prompt.clone(),
            tool_profile: input.primitive.tool_profile.clone(),
            reasoning_effort: input.reasoning_effort,
            segments: segments.iter().map(ContextSegment::descriptor).collect(),
            request_digest,
            status: ManifestStatus::Prepared,
            estimated_usage,
            comparison_json: None,
            created_at: Utc::now(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent::{primitive, workflow::NodeOp};

    #[test]
    fn assembler_preserves_legacy_request_bytes_and_estimate() {
        let primitive = primitive::primitive_for_node_op(&NodeOp::PlanContext).unwrap();
        let tools = primitive::context_planner_profile()
            .materialize(primitive::ToolProfileCapabilities::default());
        let system = "stable system bytes";
        let prompt = "stable prompt bytes";
        let mut metadata = BTreeMap::new();
        metadata.insert("task_id".to_string(), "task-1".to_string());

        let assembled = assemble(RequestAssembly {
            primitive,
            system_segments: vec![ContextSegment::new(
                "system",
                0,
                ContextRole::System,
                ContextCategory::Constraints,
                ContextRepresentation::Raw,
                "context_planner",
                1,
                system,
                CachePolicy::Request,
            )],
            user_segments: vec![ContextSegment::new(
                "prompt",
                1,
                ContextRole::Task,
                ContextCategory::TaskGoal,
                ContextRepresentation::Generated,
                "context_planner",
                1,
                prompt,
                CachePolicy::NoStore,
            )],
            tools: &tools,
            purpose: ModelPurpose::AgentPlanner,
            max_output_tokens: 512,
            reasoning_effort: Some("low".to_string()),
            correlation: RequestCorrelation {
                task_id: "task-1".to_string(),
                step_index: 2,
                node_id: Some("n2".to_string()),
                workflow_compiler_version: Some("phase1_compat_v1".to_string()),
            },
            metadata: metadata.clone(),
        })
        .unwrap();

        assert_eq!(
            assembled.request,
            ModelRequest {
                purpose: ModelPurpose::AgentPlanner,
                system: Some(system.to_string()),
                prompt: prompt.to_string(),
                estimated_tokens: EstimatedTokenUsage {
                    input: estimate_tokens(system.as_bytes())
                        + estimate_tokens(prompt.as_bytes())
                        + estimate_tool_tokens(&tools),
                    output: 512,
                },
                max_output_tokens: Some(512),
                reasoning_effort: Some("low".to_string()),
                metadata,
            }
        );
        assert_eq!(assembled.manifest.segments.len(), tools.len() + 2);
        assert_eq!(assembled.manifest.segments[0].role, ContextRole::System);
        assert_eq!(
            assembled.manifest.segments[1].role,
            ContextRole::ToolDefinition
        );
        assert_eq!(
            assembled.manifest.segments.last().unwrap().role,
            ContextRole::Task
        );
        assert_eq!(assembled.manifest.status, ManifestStatus::Prepared);
    }

    #[test]
    fn identical_requests_have_identical_digests() {
        let primitive = primitive::primitive_for_node_op(&NodeOp::DirectAnswer).unwrap();
        let build = || {
            assemble(RequestAssembly {
                primitive,
                system_segments: Vec::new(),
                user_segments: vec![ContextSegment::new(
                    "prompt",
                    0,
                    ContextRole::Task,
                    ContextCategory::TaskGoal,
                    ContextRepresentation::Raw,
                    "direct_answer",
                    1,
                    "same prompt",
                    CachePolicy::NoStore,
                )],
                tools: &[],
                purpose: ModelPurpose::FinalReport,
                max_output_tokens: 512,
                reasoning_effort: Some("low".to_string()),
                correlation: RequestCorrelation {
                    task_id: "task-1".to_string(),
                    step_index: 1,
                    node_id: Some("n1".to_string()),
                    workflow_compiler_version: None,
                },
                metadata: BTreeMap::new(),
            })
            .unwrap()
        };

        assert_eq!(
            build().manifest.request_digest,
            build().manifest.request_digest
        );
    }
}
