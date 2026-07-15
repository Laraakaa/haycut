use std::{io, path::Path, time::Instant};

use chrono::Utc;

use crate::{
    model::{ModelProvider, ModelResponse, ToolDefinition},
    store::{self, NewRequestManifest, RequestManifestCompletion},
};

use super::request::{AssembledRequest, ManifestStatus};

#[derive(Debug)]
pub struct InvocationResult<T> {
    pub value: T,
    pub manifest_id: String,
}

pub fn invoke_plain<P: ModelProvider>(
    db_path: &Path,
    provider: &P,
    assembled: AssembledRequest,
    model: &str,
    billed: bool,
    comparison_json: Option<&str>,
) -> io::Result<InvocationResult<ModelResponse>> {
    let manifest_id = prepare(db_path, &assembled, model, billed)?;
    let started = Instant::now();
    match provider.complete(assembled.request) {
        Ok(response) => {
            complete(db_path, &manifest_id, &response, started, comparison_json)?;
            Ok(InvocationResult {
                value: response,
                manifest_id,
            })
        }
        Err(error) => {
            fail(
                db_path,
                &manifest_id,
                started,
                &error.to_string(),
                comparison_json,
            )?;
            Err(io::Error::other(error.to_string()))
        }
    }
}

pub fn invoke_with_tools<P: ModelProvider>(
    db_path: &Path,
    provider: &P,
    assembled: AssembledRequest,
    tools: &[ToolDefinition],
    model: &str,
    billed: bool,
    comparison_json: Option<&str>,
) -> io::Result<InvocationResult<(String, serde_json::Value, ModelResponse)>> {
    let manifest_id = prepare(db_path, &assembled, model, billed)?;
    let started = Instant::now();
    match provider.complete_with_tools(assembled.request, tools) {
        Ok((tool, arguments, response)) => {
            complete(db_path, &manifest_id, &response, started, comparison_json)?;
            Ok(InvocationResult {
                value: (tool, arguments, response),
                manifest_id,
            })
        }
        Err(error) => {
            fail(
                db_path,
                &manifest_id,
                started,
                &error.to_string(),
                comparison_json,
            )?;
            Err(io::Error::other(error.to_string()))
        }
    }
}

fn prepare(
    db_path: &Path,
    assembled: &AssembledRequest,
    model: &str,
    billed: bool,
) -> io::Result<String> {
    store::insert_prepared_request_manifest(
        db_path,
        &NewRequestManifest {
            draft: &assembled.manifest,
            model,
            billed,
        },
    )?;
    Ok(assembled.manifest.id.clone())
}

fn complete(
    db_path: &Path,
    manifest_id: &str,
    response: &ModelResponse,
    started: Instant,
    comparison_json: Option<&str>,
) -> io::Result<()> {
    let provider_request_id = response
        .metadata
        .get("request_id")
        .or_else(|| response.metadata.get("id"))
        .map(String::as_str);
    store::finalize_request_manifest(
        db_path,
        manifest_id,
        &RequestManifestCompletion {
            status: ManifestStatus::Completed,
            reported_input_tokens: response.reported_tokens.input.map(|value| value as i64),
            reported_output_tokens: response.reported_tokens.output.map(|value| value as i64),
            reported_cached_input_tokens: response
                .reported_tokens
                .cached_input
                .map(|value| value as i64),
            provider_request_id,
            latency_ms: elapsed_millis(started),
            error_summary: None,
            completed_at: &Utc::now().to_rfc3339(),
            comparison_json,
        },
    )
    .map_err(|error| {
        io::Error::other(format!(
            "model response received but manifest {manifest_id} could not be finalized: {error}"
        ))
    })
}

fn fail(
    db_path: &Path,
    manifest_id: &str,
    started: Instant,
    error_summary: &str,
    comparison_json: Option<&str>,
) -> io::Result<()> {
    store::finalize_request_manifest(
        db_path,
        manifest_id,
        &RequestManifestCompletion {
            status: ManifestStatus::ProviderFailed,
            reported_input_tokens: None,
            reported_output_tokens: None,
            reported_cached_input_tokens: None,
            provider_request_id: None,
            latency_ms: elapsed_millis(started),
            error_summary: Some(error_summary),
            completed_at: &Utc::now().to_rfc3339(),
            comparison_json,
        },
    )
}

fn elapsed_millis(started: Instant) -> i64 {
    started.elapsed().as_millis().min(i64::MAX as u128) as i64
}
