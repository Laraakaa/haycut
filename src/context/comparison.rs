use std::{collections::BTreeSet, time::Duration};

use serde::{Deserialize, Serialize};

use crate::commands::agent::primitive::ContextCategory;

use super::{compiler::CompiledContext, request::RequestSegmentDescriptor};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonVerdict {
    Pass,
    Fail,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextCompilationComparison {
    pub required_categories_present: Vec<ContextCategory>,
    pub required_categories_missing: Vec<ContextCategory>,
    pub source_identities_only_legacy: Vec<String>,
    pub source_identities_only_compiled: Vec<String>,
    pub selected_artifact_digests: Vec<String>,
    pub stale_artifact_count: usize,
    pub duplicate_digest_count: usize,
    pub category_order_violations: usize,
    pub legacy_tokens: usize,
    pub compiled_tokens: usize,
    pub unresolved_requirements: Vec<String>,
    pub compiler_duration_ms: u64,
    pub authoritative: bool,
    pub rollback_available: bool,
    pub verdict: ComparisonVerdict,
    pub reasons: Vec<String>,
}

pub fn compare(
    required_categories: &[ContextCategory],
    legacy_segments: &[RequestSegmentDescriptor],
    compiled: &CompiledContext,
    duration: Duration,
) -> ContextCompilationComparison {
    let compiled_categories: BTreeSet<_> = compiled
        .selected_artifacts
        .iter()
        .map(|artifact| artifact.category)
        .collect();
    let required_categories_present: Vec<_> = required_categories
        .iter()
        .copied()
        .filter(|category| compiled_categories.contains(category))
        .collect();
    let required_categories_missing: Vec<_> = required_categories
        .iter()
        .copied()
        .filter(|category| !compiled_categories.contains(category))
        .collect();
    let legacy_digests: BTreeSet<_> = legacy_segments
        .iter()
        .map(|segment| segment.content_digest.clone())
        .collect();
    let compiled_digests: BTreeSet<_> = compiled
        .selected_artifacts
        .iter()
        .map(|artifact| artifact.content_digest.clone())
        .collect();
    let mut seen = BTreeSet::new();
    let duplicate_digest_count = compiled
        .selected_artifacts
        .iter()
        .filter(|artifact| !seen.insert(artifact.content_digest.clone()))
        .count();
    let category_order_violations = compiled
        .selected_artifacts
        .windows(2)
        .filter(|pair| pair[0].category > pair[1].category)
        .count();
    let stale_artifact_count = compiled
        .selected_artifacts
        .iter()
        .filter(|artifact| !artifact.freshness.is_fresh())
        .count();
    let unresolved_requirements: Vec<_> = compiled
        .unresolved_requirements
        .iter()
        .map(|requirement| format!("{:?}: {}", requirement.category, requirement.reason))
        .collect();
    let mut reasons = Vec::new();
    if !required_categories_missing.is_empty() {
        reasons.push("required categories are missing".to_string());
    }
    if stale_artifact_count > 0 {
        reasons.push("compiled context contains stale artifacts".to_string());
    }
    if duplicate_digest_count > 0 {
        reasons.push("compiled context contains duplicate digests".to_string());
    }
    if category_order_violations > 0 {
        reasons.push("compiled category ordering is unstable".to_string());
    }
    if !unresolved_requirements.is_empty() {
        reasons.push("context requirements remain unresolved".to_string());
    }
    let verdict = if reasons.is_empty() {
        ComparisonVerdict::Pass
    } else {
        ComparisonVerdict::Fail
    };

    ContextCompilationComparison {
        required_categories_present,
        required_categories_missing,
        source_identities_only_legacy: legacy_digests
            .difference(&compiled_digests)
            .cloned()
            .collect(),
        source_identities_only_compiled: compiled_digests
            .difference(&legacy_digests)
            .cloned()
            .collect(),
        selected_artifact_digests: compiled_digests.into_iter().collect(),
        stale_artifact_count,
        duplicate_digest_count,
        category_order_violations,
        legacy_tokens: legacy_segments
            .iter()
            .map(|segment| segment.estimated_tokens)
            .sum(),
        compiled_tokens: compiled.total_tokens,
        unresolved_requirements,
        compiler_duration_ms: duration.as_millis().min(u64::MAX as u128) as u64,
        authoritative: false,
        rollback_available: true,
        verdict,
        reasons,
    }
}

pub fn compiler_error(
    required_categories: &[ContextCategory],
    legacy_segments: &[RequestSegmentDescriptor],
    duration: Duration,
    error: &str,
) -> ContextCompilationComparison {
    ContextCompilationComparison {
        required_categories_present: Vec::new(),
        required_categories_missing: required_categories.to_vec(),
        source_identities_only_legacy: legacy_segments
            .iter()
            .map(|segment| segment.content_digest.clone())
            .collect(),
        source_identities_only_compiled: Vec::new(),
        selected_artifact_digests: Vec::new(),
        stale_artifact_count: 0,
        duplicate_digest_count: 0,
        category_order_violations: 0,
        legacy_tokens: legacy_segments
            .iter()
            .map(|segment| segment.estimated_tokens)
            .sum(),
        compiled_tokens: 0,
        unresolved_requirements: vec![error.to_string()],
        compiler_duration_ms: duration.as_millis().min(u64::MAX as u128) as u64,
        authoritative: false,
        rollback_available: true,
        verdict: ComparisonVerdict::Fail,
        reasons: vec![format!("context compiler failed: {error}")],
    }
}
