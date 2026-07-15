use std::{collections::BTreeSet, io, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    commands::{agent::primitive::PrimitiveSpec, task::TaskState},
    context::request::ContextRepresentation,
};

use super::{
    adapters,
    artifact::{ContextArtifact, RequirementKind},
    digest::{DIGEST_SCHEMA_VERSION, digest_json},
};

pub const CONTEXT_COMPILER_VERSION: &str = "context_compiler_v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OmittedArtifact {
    pub artifact_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnresolvedRequirement {
    pub category: crate::commands::agent::primitive::ContextCategory,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompiledContext {
    pub compiler_version: String,
    pub primitive_id: String,
    pub primitive_version: u16,
    pub selected_artifacts: Vec<ContextArtifact>,
    pub omitted_candidates: Vec<OmittedArtifact>,
    pub unresolved_requirements: Vec<UnresolvedRequirement>,
    pub total_bytes: usize,
    pub total_tokens: usize,
    pub bundle_digest: String,
}

#[derive(Serialize)]
struct BundleIdentity<'a> {
    compiler_version: &'a str,
    primitive_id: &'a str,
    primitive_version: u16,
    artifact_ids: Vec<&'a str>,
}

pub fn compile(
    task: &TaskState,
    primitive: &PrimitiveSpec,
    repository_root: &Path,
    token_budget: usize,
) -> io::Result<CompiledContext> {
    let adapted = adapters::from_task(task, repository_root)?;
    compile_artifacts(primitive, adapted.artifacts, token_budget).map_err(io::Error::other)
}

pub fn compile_artifacts(
    primitive: &PrimitiveSpec,
    mut artifacts: Vec<ContextArtifact>,
    token_budget: usize,
) -> Result<CompiledContext, serde_json::Error> {
    artifacts.sort_by_key(|artifact| {
        (
            artifact.category,
            representation_rank(artifact.representation),
            artifact.id.clone(),
        )
    });
    let mut selected = Vec::new();
    let mut omitted = Vec::new();
    let mut unresolved = Vec::new();
    let mut selected_digests = BTreeSet::new();
    let mut total_tokens = 0usize;

    for requirement in &primitive.context_requirements {
        let candidates: Vec<_> = artifacts
            .iter()
            .filter(|artifact| artifact.category == requirement.category)
            .filter(|artifact| {
                requirement
                    .allowed_representations
                    .contains(&artifact.representation)
            })
            .collect();
        let mut selected_for_requirement = 0usize;
        for artifact in candidates {
            if !artifact.freshness.is_fresh() {
                omitted.push(OmittedArtifact {
                    artifact_id: artifact.id.clone(),
                    reason: "stale_or_missing_dependency".to_string(),
                });
                continue;
            }
            if !selected_digests.insert(artifact.content_digest.clone()) {
                omitted.push(OmittedArtifact {
                    artifact_id: artifact.id.clone(),
                    reason: "duplicate_content_digest".to_string(),
                });
                continue;
            }
            if requirement
                .maximum_cardinality
                .is_some_and(|maximum| selected_for_requirement >= maximum)
            {
                omitted.push(OmittedArtifact {
                    artifact_id: artifact.id.clone(),
                    reason: "maximum_cardinality".to_string(),
                });
                continue;
            }
            if requirement
                .maximum_tokens
                .is_some_and(|maximum| artifact.estimated_tokens > maximum)
            {
                omitted.push(OmittedArtifact {
                    artifact_id: artifact.id.clone(),
                    reason: "requirement_token_limit".to_string(),
                });
                continue;
            }
            if total_tokens.saturating_add(artifact.estimated_tokens) > token_budget {
                omitted.push(OmittedArtifact {
                    artifact_id: artifact.id.clone(),
                    reason: "context_budget".to_string(),
                });
                continue;
            }
            total_tokens = total_tokens.saturating_add(artifact.estimated_tokens);
            selected_for_requirement += 1;
            selected.push(artifact.clone());
        }
        if selected_for_requirement < requirement.minimum_cardinality {
            unresolved.push(UnresolvedRequirement {
                category: requirement.category,
                reason: if requirement.kind == RequirementKind::Required {
                    "required category could not be resolved with fresh artifacts".to_string()
                } else {
                    "minimum cardinality not met".to_string()
                },
            });
        }
    }

    selected.sort_by_key(|artifact| {
        (
            artifact.category,
            representation_rank(artifact.representation),
            artifact.id.clone(),
        )
    });

    let bundle_digest = digest_json(
        "compiled-context-bundle",
        DIGEST_SCHEMA_VERSION,
        &BundleIdentity {
            compiler_version: CONTEXT_COMPILER_VERSION,
            primitive_id: primitive.id.as_str(),
            primitive_version: primitive.version.get(),
            artifact_ids: selected
                .iter()
                .map(|artifact| artifact.id.as_str())
                .collect(),
        },
    )?;
    Ok(CompiledContext {
        compiler_version: CONTEXT_COMPILER_VERSION.to_string(),
        primitive_id: primitive.id.to_string(),
        primitive_version: primitive.version.get(),
        total_bytes: selected.iter().map(|artifact| artifact.byte_size).sum(),
        total_tokens,
        selected_artifacts: selected,
        omitted_candidates: omitted,
        unresolved_requirements: unresolved,
        bundle_digest,
    })
}

fn representation_rank(representation: ContextRepresentation) -> u8 {
    match representation {
        ContextRepresentation::Raw => 0,
        ContextRepresentation::Extracted => 1,
        ContextRepresentation::Compressed => 2,
        ContextRepresentation::Generated => 3,
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        commands::agent::{primitive, workflow::NodeOp},
        context::artifact::{
            ArtifactDependency, ContextRequirement, DependencySnapshot, task_field_digest,
        },
    };

    use super::*;

    fn artifact(
        category: crate::commands::agent::primitive::ContextCategory,
        representation: ContextRepresentation,
        content: &str,
        fresh: bool,
    ) -> ContextArtifact {
        let dependency = ArtifactDependency::TaskField {
            task_id: "task-1".to_string(),
            field: format!("{category:?}"),
            field_digest: task_field_digest(content.as_bytes()),
        };
        let mut snapshot = DependencySnapshot::default();
        if fresh {
            snapshot.insert(dependency.key(), dependency.expected());
        }
        ContextArtifact::new(
            category,
            representation,
            "test",
            1,
            content,
            vec![dependency],
            None,
            &snapshot,
        )
        .unwrap()
    }

    fn direct_answer_spec() -> PrimitiveSpec {
        primitive::primitive_for_node_op(&NodeOp::DirectAnswer)
            .unwrap()
            .clone()
    }

    #[test]
    fn bundle_digest_and_order_are_stable() {
        let spec = direct_answer_spec();
        let goal = artifact(
            crate::commands::agent::primitive::ContextCategory::TaskGoal,
            ContextRepresentation::Raw,
            "goal",
            true,
        );
        let observation = artifact(
            crate::commands::agent::primitive::ContextCategory::Observations,
            ContextRepresentation::Extracted,
            "observation",
            true,
        );

        let first = compile_artifacts(&spec, vec![goal.clone(), observation.clone()], 100).unwrap();
        let second = compile_artifacts(&spec, vec![observation, goal], 100).unwrap();

        assert_eq!(first.bundle_digest, second.bundle_digest);
        assert_eq!(
            first
                .selected_artifacts
                .iter()
                .map(|artifact| &artifact.id)
                .collect::<Vec<_>>(),
            second
                .selected_artifacts
                .iter()
                .map(|artifact| &artifact.id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn missing_dependency_fails_required_resolution() {
        let spec = direct_answer_spec();
        let stale_goal = artifact(
            crate::commands::agent::primitive::ContextCategory::TaskGoal,
            ContextRepresentation::Raw,
            "goal",
            false,
        );

        let compiled = compile_artifacts(&spec, vec![stale_goal], 100).unwrap();

        assert_eq!(compiled.unresolved_requirements.len(), 1);
        assert!(compiled.selected_artifacts.is_empty());
        assert_eq!(
            compiled.omitted_candidates[0].reason,
            "stale_or_missing_dependency"
        );
    }

    #[test]
    fn deduplicates_content_and_prefers_raw_representation() {
        let mut spec = direct_answer_spec();
        spec.context_requirements = vec![ContextRequirement::required(
            crate::commands::agent::primitive::ContextCategory::TaskGoal,
            0,
        )];
        let raw = artifact(
            crate::commands::agent::primitive::ContextCategory::TaskGoal,
            ContextRepresentation::Raw,
            "same",
            true,
        );
        let extracted = artifact(
            crate::commands::agent::primitive::ContextCategory::TaskGoal,
            ContextRepresentation::Extracted,
            "same",
            true,
        );

        let compiled = compile_artifacts(&spec, vec![extracted, raw], 100).unwrap();

        assert_eq!(compiled.selected_artifacts.len(), 1);
        assert_eq!(
            compiled.selected_artifacts[0].representation,
            ContextRepresentation::Raw
        );
        assert!(
            compiled
                .omitted_candidates
                .iter()
                .any(|omitted| omitted.reason == "duplicate_content_digest")
        );
    }

    #[test]
    fn budget_keeps_required_artifact_and_omits_optional() {
        let spec = direct_answer_spec();
        let goal = artifact(
            crate::commands::agent::primitive::ContextCategory::TaskGoal,
            ContextRepresentation::Raw,
            "goal",
            true,
        );
        let observation = artifact(
            crate::commands::agent::primitive::ContextCategory::Observations,
            ContextRepresentation::Extracted,
            "a very long optional observation that exceeds the remaining budget",
            true,
        );

        let compiled = compile_artifacts(
            &spec,
            vec![observation, goal.clone()],
            goal.estimated_tokens,
        )
        .unwrap();

        assert_eq!(compiled.selected_artifacts.len(), 1);
        assert_eq!(compiled.selected_artifacts[0].category, goal.category);
        assert!(compiled.unresolved_requirements.is_empty());
    }
}
