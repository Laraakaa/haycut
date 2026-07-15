use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{commands::agent::primitive::ContextCategory, util::estimate_tokens};

use super::{
    digest::{DIGEST_SCHEMA_VERSION, digest_bytes, digest_json},
    request::ContextRepresentation,
};

pub const CONTEXT_ARTIFACT_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementKind {
    Required,
    Optional,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessPolicy {
    Current,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextRequirement {
    pub category: ContextCategory,
    pub kind: RequirementKind,
    pub minimum_cardinality: usize,
    pub maximum_cardinality: Option<usize>,
    pub allowed_representations: Vec<ContextRepresentation>,
    pub freshness_policy: FreshnessPolicy,
    pub maximum_tokens: Option<usize>,
    pub priority: u16,
    pub ordering_group: u16,
}

impl ContextRequirement {
    pub fn required(category: ContextCategory, ordering_group: u16) -> Self {
        Self::new(category, RequirementKind::Required, 1, ordering_group)
    }

    pub fn optional(category: ContextCategory, ordering_group: u16) -> Self {
        Self::new(category, RequirementKind::Optional, 0, ordering_group)
    }

    fn new(
        category: ContextCategory,
        kind: RequirementKind,
        minimum_cardinality: usize,
        ordering_group: u16,
    ) -> Self {
        Self {
            category,
            kind,
            minimum_cardinality,
            maximum_cardinality: None,
            allowed_representations: vec![
                ContextRepresentation::Raw,
                ContextRepresentation::Extracted,
                ContextRepresentation::Compressed,
                ContextRepresentation::Generated,
            ],
            freshness_policy: FreshnessPolicy::Current,
            maximum_tokens: None,
            priority: ordering_group,
            ordering_group,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactDependency {
    File {
        path: String,
        content_digest: String,
    },
    Run {
        run_id: String,
        #[serde(default = "default_run_packet_kind")]
        packet_kind: String,
        packet_digest: String,
    },
    TaskField {
        task_id: String,
        field: String,
        field_digest: String,
    },
    CodeGraph {
        input_digest: String,
    },
    Producer {
        id: String,
        version: u16,
    },
    Configuration {
        digest: String,
    },
    ToolProfile {
        id: String,
        version: u16,
    },
    Prompt {
        id: String,
        version: u16,
    },
}

fn default_run_packet_kind() -> String {
    "packet".to_string()
}

impl ArtifactDependency {
    pub fn key(&self) -> String {
        match self {
            Self::File { path, .. } => format!("file:{path}"),
            Self::Run {
                run_id,
                packet_kind,
                ..
            } => format!("run:{run_id}:{packet_kind}"),
            Self::TaskField { task_id, field, .. } => format!("task:{task_id}:{field}"),
            Self::CodeGraph { .. } => "code_graph".to_string(),
            Self::Producer { id, .. } => format!("producer:{id}"),
            Self::Configuration { .. } => "configuration".to_string(),
            Self::ToolProfile { id, .. } => format!("tool_profile:{id}"),
            Self::Prompt { id, .. } => format!("prompt:{id}"),
        }
    }

    pub fn expected(&self) -> String {
        match self {
            Self::File { content_digest, .. } => content_digest.clone(),
            Self::Run { packet_digest, .. } => packet_digest.clone(),
            Self::TaskField { field_digest, .. } => field_digest.clone(),
            Self::CodeGraph { input_digest }
            | Self::Configuration {
                digest: input_digest,
            } => input_digest.clone(),
            Self::Producer { version, .. }
            | Self::ToolProfile { version, .. }
            | Self::Prompt { version, .. } => version.to_string(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DependencySnapshot {
    values: BTreeMap<String, String>,
}

impl DependencySnapshot {
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values.insert(key.into(), value.into());
    }

    pub fn freshness(&self, dependencies: &[ArtifactDependency]) -> FreshnessResult {
        for dependency in dependencies {
            let key = dependency.key();
            let Some(actual) = self.values.get(&key) else {
                return FreshnessResult::Missing { dependency: key };
            };
            if *actual != dependency.expected() {
                return FreshnessResult::Stale {
                    dependency: key,
                    expected: dependency.expected(),
                    actual: actual.clone(),
                };
            }
        }
        FreshnessResult::Fresh
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FreshnessResult {
    Fresh,
    Stale {
        dependency: String,
        expected: String,
        actual: String,
    },
    Missing {
        dependency: String,
    },
}

impl FreshnessResult {
    pub fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextArtifact {
    pub id: String,
    pub category: ContextCategory,
    pub representation: ContextRepresentation,
    pub schema_version: u16,
    pub producer_id: String,
    pub producer_version: u16,
    pub content: String,
    pub content_digest: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provenance: BTreeMap<String, String>,
    pub dependencies: Vec<ArtifactDependency>,
    pub repository_identity: Option<String>,
    pub freshness: FreshnessResult,
    pub byte_size: usize,
    pub estimated_tokens: usize,
}

#[derive(Serialize)]
struct ArtifactIdentity<'a> {
    category: ContextCategory,
    representation: ContextRepresentation,
    schema_version: u16,
    producer_id: &'a str,
    producer_version: u16,
    content_digest: &'a str,
    dependencies: &'a [ArtifactDependency],
    repository_identity: &'a Option<String>,
}

impl ContextArtifact {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        category: ContextCategory,
        representation: ContextRepresentation,
        producer_id: impl Into<String>,
        producer_version: u16,
        content: impl Into<String>,
        dependencies: Vec<ArtifactDependency>,
        repository_identity: Option<String>,
        snapshot: &DependencySnapshot,
    ) -> Result<Self, serde_json::Error> {
        let content = content.into();
        let producer_id = producer_id.into();
        let content_digest = digest_bytes(
            "context-artifact-content",
            CONTEXT_ARTIFACT_SCHEMA_VERSION,
            content.as_bytes(),
        );
        let id = digest_json(
            "context-artifact",
            DIGEST_SCHEMA_VERSION,
            &ArtifactIdentity {
                category,
                representation,
                schema_version: CONTEXT_ARTIFACT_SCHEMA_VERSION,
                producer_id: &producer_id,
                producer_version,
                content_digest: &content_digest,
                dependencies: &dependencies,
                repository_identity: &repository_identity,
            },
        )?;
        Ok(Self {
            id,
            category,
            representation,
            schema_version: CONTEXT_ARTIFACT_SCHEMA_VERSION,
            producer_id,
            producer_version,
            content_digest,
            provenance: BTreeMap::new(),
            freshness: snapshot.freshness(&dependencies),
            byte_size: content.len(),
            estimated_tokens: estimate_tokens(content.as_bytes()),
            content,
            dependencies,
            repository_identity,
        })
    }

    #[cfg(test)]
    pub fn freshness_against(&self, snapshot: &DependencySnapshot) -> FreshnessResult {
        snapshot.freshness(&self.dependencies)
    }
}

pub fn task_field_digest(bytes: &[u8]) -> String {
    digest_bytes("task-field", DIGEST_SCHEMA_VERSION, bytes)
}

pub fn file_content_digest(bytes: &[u8]) -> String {
    digest_bytes("file-content", DIGEST_SCHEMA_VERSION, bytes)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn artifact_identity_is_stable() {
        let dependency = ArtifactDependency::TaskField {
            task_id: "task-1".to_string(),
            field: "goal".to_string(),
            field_digest: task_field_digest(b"goal"),
        };
        let mut snapshot = DependencySnapshot::default();
        snapshot.insert(dependency.key(), dependency.expected());
        let build = || {
            ContextArtifact::new(
                ContextCategory::TaskGoal,
                ContextRepresentation::Raw,
                "task_state",
                1,
                "goal",
                vec![dependency.clone()],
                None,
                &snapshot,
            )
            .unwrap()
        };

        assert_eq!(build().id, build().id);
        assert_eq!(build().content_digest, build().content_digest);
    }

    #[test]
    fn file_mutation_invalidates_artifact() {
        let root = std::env::temp_dir().join(format!(
            "haycut-artifact-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("lib.rs");
        fs::write(&path, "fn first() {}\n").unwrap();
        let expected = file_content_digest(&fs::read(&path).unwrap());
        let dependency = ArtifactDependency::File {
            path: "lib.rs".to_string(),
            content_digest: expected.clone(),
        };
        let mut initial = DependencySnapshot::default();
        initial.insert(dependency.key(), expected);
        let artifact = ContextArtifact::new(
            ContextCategory::RelevantSymbol,
            ContextRepresentation::Extracted,
            "symbol_extractor",
            1,
            "fn first() {}",
            vec![dependency.clone()],
            Some(root.display().to_string()),
            &initial,
        )
        .unwrap();
        assert!(artifact.freshness.is_fresh());

        fs::write(&path, "fn second() {}\n").unwrap();
        let mut changed = DependencySnapshot::default();
        changed.insert(
            dependency.key(),
            file_content_digest(&fs::read(&path).unwrap()),
        );

        assert!(matches!(
            artifact.freshness_against(&changed),
            FreshnessResult::Stale { .. }
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_and_producer_changes_invalidate_artifacts() {
        let task_dependency = ArtifactDependency::TaskField {
            task_id: "task-1".to_string(),
            field: "failure".to_string(),
            field_digest: task_field_digest(b"old failure"),
        };
        let producer_dependency = ArtifactDependency::Producer {
            id: "evidence_adapter".to_string(),
            version: 1,
        };
        let mut initial = DependencySnapshot::default();
        initial.insert(task_dependency.key(), task_dependency.expected());
        initial.insert(producer_dependency.key(), producer_dependency.expected());
        let artifact = ContextArtifact::new(
            ContextCategory::FailureEvidence,
            ContextRepresentation::Extracted,
            "evidence_adapter",
            1,
            "old failure",
            vec![task_dependency.clone(), producer_dependency.clone()],
            None,
            &initial,
        )
        .unwrap();

        let mut changed_task = initial.clone();
        changed_task.insert(task_dependency.key(), task_field_digest(b"new failure"));
        assert!(matches!(
            artifact.freshness_against(&changed_task),
            FreshnessResult::Stale { .. }
        ));

        let mut changed_producer = initial;
        changed_producer.insert(producer_dependency.key(), "2");
        assert!(matches!(
            artifact.freshness_against(&changed_producer),
            FreshnessResult::Stale { .. }
        ));
    }

    #[test]
    fn missing_dependency_is_not_fresh() {
        let dependency = ArtifactDependency::Run {
            run_id: "run-1".to_string(),
            packet_kind: "evidence".to_string(),
            packet_digest: "packet-digest".to_string(),
        };
        let snapshot = DependencySnapshot::default();
        assert!(matches!(
            snapshot.freshness(&[dependency]),
            FreshnessResult::Missing { .. }
        ));
    }

    #[test]
    fn evidence_changes_invalidate_failure_artifacts() {
        let evidence = ArtifactDependency::Run {
            run_id: "run-1".to_string(),
            packet_kind: "evidence".to_string(),
            packet_digest: "evidence-v1".to_string(),
        };
        let compact = ArtifactDependency::Run {
            run_id: "run-1".to_string(),
            packet_kind: "compact".to_string(),
            packet_digest: "compact-v1".to_string(),
        };
        let mut snapshot = DependencySnapshot::default();
        snapshot.insert(evidence.key(), evidence.expected());
        snapshot.insert(compact.key(), compact.expected());
        let artifact = ContextArtifact::new(
            ContextCategory::FailureEvidence,
            ContextRepresentation::Extracted,
            "stored_run",
            1,
            "assertion failed",
            vec![evidence.clone()],
            None,
            &snapshot,
        )
        .unwrap();
        assert!(artifact.freshness.is_fresh());

        snapshot.insert(evidence.key(), "evidence-v2");
        assert!(matches!(
            artifact.freshness_against(&snapshot),
            FreshnessResult::Stale { .. }
        ));
        assert_eq!(snapshot.freshness(&[compact]), FreshnessResult::Fresh);
    }
}
