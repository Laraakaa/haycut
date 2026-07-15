use std::{fs, io, path::Path};

use crate::{
    commands::{agent::primitive::ContextCategory, task::TaskState},
    store::{self, RUN_STORE_PATH},
};

use super::{
    artifact::{
        ArtifactDependency, ContextArtifact, DependencySnapshot, file_content_digest,
        task_field_digest,
    },
    request::ContextRepresentation,
};

pub struct AdaptedContext {
    pub artifacts: Vec<ContextArtifact>,
}

pub fn from_task(task: &TaskState, repository_root: &Path) -> io::Result<AdaptedContext> {
    let mut snapshot = DependencySnapshot::default();
    let mut artifacts = Vec::new();
    push_task_field(
        &mut artifacts,
        &mut snapshot,
        task,
        "goal",
        ContextCategory::TaskGoal,
        ContextRepresentation::Raw,
        task.goal.clone(),
    )?;
    push_task_field(
        &mut artifacts,
        &mut snapshot,
        task,
        "acceptance",
        ContextCategory::AcceptanceCriteria,
        ContextRepresentation::Raw,
        serde_json::to_string(&task.acceptance).map_err(io::Error::other)?,
    )?;
    push_task_field(
        &mut artifacts,
        &mut snapshot,
        task,
        "constraints",
        ContextCategory::Constraints,
        ContextRepresentation::Raw,
        serde_json::to_string(&task.constraints).map_err(io::Error::other)?,
    )?;
    push_task_field(
        &mut artifacts,
        &mut snapshot,
        task,
        "budget",
        ContextCategory::Budget,
        ContextRepresentation::Extracted,
        serde_json::to_string(&task.budget).map_err(io::Error::other)?,
    )?;
    if let Some(intent) = task.intent {
        push_task_field(
            &mut artifacts,
            &mut snapshot,
            task,
            "intent",
            ContextCategory::Intent,
            ContextRepresentation::Extracted,
            serde_json::to_string(&intent).map_err(io::Error::other)?,
        )?;
    }
    if let Some(project) = &task.project {
        push_task_field(
            &mut artifacts,
            &mut snapshot,
            task,
            "project",
            ContextCategory::ProjectEnvironment,
            ContextRepresentation::Extracted,
            serde_json::to_string(project).map_err(io::Error::other)?,
        )?;
    }
    if let Some(verification) = &task.verification {
        push_task_field(
            &mut artifacts,
            &mut snapshot,
            task,
            "verification",
            ContextCategory::VerificationPlan,
            ContextRepresentation::Extracted,
            serde_json::to_string(verification).map_err(io::Error::other)?,
        )?;
    }
    if let Some(failure) = &task.current_failure {
        push_task_field(
            &mut artifacts,
            &mut snapshot,
            task,
            "current_failure",
            ContextCategory::CurrentFailure,
            ContextRepresentation::Extracted,
            serde_json::to_string(failure).map_err(io::Error::other)?,
        )?;
    }
    for observation in &task.observations {
        let category = if observation.kind == "off_site_symbol" {
            ContextCategory::RelevantSymbol
        } else if observation.source == "agent:read_context"
            && observation.summary.starts_with("File:")
        {
            ContextCategory::RelevantWindow
        } else if observation.kind.contains("failure") || observation.source.starts_with("run:") {
            ContextCategory::FailureEvidence
        } else {
            ContextCategory::Observations
        };
        push_task_field(
            &mut artifacts,
            &mut snapshot,
            task,
            &format!("observation:{}", observation.id),
            category,
            ContextRepresentation::Extracted,
            observation.summary.clone(),
        )?;
    }
    for hypothesis in &task.hypotheses {
        push_task_field(
            &mut artifacts,
            &mut snapshot,
            task,
            &format!("hypothesis:{}", hypothesis.id),
            ContextCategory::Hypotheses,
            ContextRepresentation::Extracted,
            hypothesis.summary.clone(),
        )?;
    }
    if !task.next_actions.is_empty() {
        push_task_field(
            &mut artifacts,
            &mut snapshot,
            task,
            "next_actions",
            ContextCategory::QueuedActions,
            ContextRepresentation::Generated,
            serde_json::to_string(&task.next_actions).map_err(io::Error::other)?,
        )?;
    }
    if !task.route.is_empty() {
        push_task_field(
            &mut artifacts,
            &mut snapshot,
            task,
            "route",
            ContextCategory::RouteHistory,
            ContextRepresentation::Extracted,
            serde_json::to_string(&task.route).map_err(io::Error::other)?,
        )?;
    }
    if let Some(patch) = &task.patch_text {
        push_task_field(
            &mut artifacts,
            &mut snapshot,
            task,
            "patch_text",
            ContextCategory::PatchPlan,
            ContextRepresentation::Generated,
            patch.clone(),
        )?;
    }

    for candidate in &task.available_context {
        let key = format!("file:{}", candidate.path);
        let current = fs::read(repository_root.join(&candidate.path))
            .map(|bytes| file_content_digest(&bytes));
        if let Ok(current) = &current {
            snapshot.insert(&key, current);
        }
        let expected = candidate
            .file_digest
            .clone()
            .unwrap_or_else(|| "missing".to_string());
        let mut artifact = ContextArtifact::new(
            ContextCategory::CodeGraphCandidate,
            ContextRepresentation::Extracted,
            "available_context",
            1,
            candidate.body.clone(),
            vec![ArtifactDependency::File {
                path: candidate.path.clone(),
                content_digest: expected.clone(),
            }],
            Some(repository_root.display().to_string()),
            &snapshot,
        )
        .map_err(io::Error::other)?;
        artifact
            .provenance
            .insert("candidate_id".to_string(), candidate.id.clone());
        artifact
            .provenance
            .insert("symbol".to_string(), candidate.symbol.clone());
        artifact
            .provenance
            .insert("path".to_string(), candidate.path.clone());
        artifact
            .provenance
            .insert("start_line".to_string(), candidate.start_line.to_string());
        artifacts.push(artifact);
        let mut available = ContextArtifact::new(
            ContextCategory::AvailableContext,
            ContextRepresentation::Extracted,
            "available_context",
            1,
            candidate.body.clone(),
            vec![ArtifactDependency::File {
                path: candidate.path.clone(),
                content_digest: expected,
            }],
            Some(repository_root.display().to_string()),
            &snapshot,
        )
        .map_err(io::Error::other)?;
        available
            .provenance
            .insert("candidate_id".to_string(), candidate.id.clone());
        available
            .provenance
            .insert("symbol".to_string(), candidate.symbol.clone());
        available
            .provenance
            .insert("path".to_string(), candidate.path.clone());
        available
            .provenance
            .insert("start_line".to_string(), candidate.start_line.to_string());
        artifacts.push(available);
    }

    let db_path = repository_root.join(RUN_STORE_PATH);
    if db_path.exists() {
        if let Ok(run) = store::latest_run(&db_path)
            && task.runs.iter().any(|task_run| task_run.id == run.id)
        {
            push_run_packet(
                &mut artifacts,
                &mut snapshot,
                &run.id,
                "evidence",
                ContextCategory::FailureEvidence,
                ContextRepresentation::Extracted,
                run.evidence_json,
            )?;
            push_run_packet(
                &mut artifacts,
                &mut snapshot,
                &run.id,
                "compact",
                ContextCategory::RecentToolOutput,
                ContextRepresentation::Compressed,
                run.compact_json,
            )?;
        }
        for file in store::largest_files(&db_path, 100)? {
            let key = format!("file:{}", file.path);
            if let Ok(bytes) = fs::read(repository_root.join(&file.path)) {
                snapshot.insert(&key, file_content_digest(&bytes));
            }
            artifacts.push(
                ContextArtifact::new(
                    ContextCategory::RepositoryInventory,
                    ContextRepresentation::Extracted,
                    "file_inventory",
                    1,
                    serde_json::to_string(&(
                        &file.path,
                        file.language.as_deref(),
                        file.byte_size,
                        file.line_count,
                        file.estimated_tokens,
                    ))
                    .map_err(io::Error::other)?,
                    vec![ArtifactDependency::File {
                        path: file.path,
                        content_digest: file.content_hash,
                    }],
                    Some(repository_root.display().to_string()),
                    &snapshot,
                )
                .map_err(io::Error::other)?,
            );
        }
    }

    Ok(AdaptedContext { artifacts })
}

fn push_run_packet(
    artifacts: &mut Vec<ContextArtifact>,
    snapshot: &mut DependencySnapshot,
    run_id: &str,
    packet_kind: &str,
    category: ContextCategory,
    representation: ContextRepresentation,
    content: String,
) -> io::Result<()> {
    let packet_digest = super::digest::digest_bytes(
        "stored-run-packet",
        super::digest::DIGEST_SCHEMA_VERSION,
        content.as_bytes(),
    );
    let dependency = ArtifactDependency::Run {
        run_id: run_id.to_string(),
        packet_kind: packet_kind.to_string(),
        packet_digest: packet_digest.clone(),
    };
    snapshot.insert(dependency.key(), packet_digest);
    artifacts.push(
        ContextArtifact::new(
            category,
            representation,
            "stored_run",
            1,
            content,
            vec![dependency],
            None,
            snapshot,
        )
        .map_err(io::Error::other)?,
    );
    Ok(())
}

fn push_task_field(
    artifacts: &mut Vec<ContextArtifact>,
    snapshot: &mut DependencySnapshot,
    task: &TaskState,
    field: &str,
    category: ContextCategory,
    representation: ContextRepresentation,
    content: String,
) -> io::Result<()> {
    let digest = task_field_digest(content.as_bytes());
    let dependency = ArtifactDependency::TaskField {
        task_id: task.id.clone(),
        field: field.to_string(),
        field_digest: digest.clone(),
    };
    snapshot.insert(dependency.key(), digest);
    artifacts.push(
        ContextArtifact::new(
            category,
            representation,
            "task_state",
            1,
            content,
            vec![dependency],
            None,
            snapshot,
        )
        .map_err(io::Error::other)?,
    );
    Ok(())
}
