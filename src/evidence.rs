//! The semantic layer: turns a compacted run plus raw output into a structured
//! [`EvidencePacket`].
//!
//! The evidence packet is HayCut's stable internal contract. `trace` stores it,
//! `report` renders it, and future commands consume it. Extraction lives in
//! [`crate::extract`]; this module owns derivation (likely failure, primary
//! diagnostic, and suggested context).

use serde::{Deserialize, Serialize};

use crate::{
    compactor::CompactPacket,
    extract::{self, Diagnostic, DiagnosticKind, FileReference, Severity},
};

/// Number of lines above and below a primary location to suggest as context.
const CONTEXT_RADIUS: usize = 10;

/// Structured evidence derived from a single captured run.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EvidencePacket {
    pub schema_version: u8,
    pub run_id: String,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub likely_failure: Option<LikelyFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_diagnostic: Option<PrimaryDiagnostic>,
    pub diagnostics: Vec<EvidenceDiagnostic>,
    pub file_refs: Vec<FileRef>,
    pub context_items: Vec<ContextItem>,
    pub token_summary: TokenSummary,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Outcome {
    pub exit_code: i32,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LikelyFailure {
    pub kind: String,
    pub summary: String,
    pub confidence: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PrimaryDiagnostic {
    pub source: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceDiagnostic {
    pub kind: DiagnosticKind,
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileRef {
    pub file: String,
    pub line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextItem {
    pub kind: String,
    pub target: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TokenSummary {
    pub raw_tokens: usize,
    pub packet_tokens: usize,
    pub saved_tokens: usize,
    pub reduction_percent: f64,
}

impl EvidencePacket {
    /// Render a compact, human-readable view of the evidence packet.
    pub fn render_text(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!("Run {}\n", self.run_id));
        output.push_str(&format!(
            "Outcome: {} (exit {})\n",
            self.outcome.status, self.outcome.exit_code
        ));

        output.push_str("\nLikely failure\n");
        match &self.likely_failure {
            Some(failure) => {
                output.push_str(&format!("  {}\n", failure.kind));
                output.push_str(&format!("  {}\n", failure.summary));
            }
            None => output.push_str("  none detected\n"),
        }

        output.push_str("\nPrimary diagnostic\n");
        match &self.primary_diagnostic {
            Some(primary) => {
                output.push_str(&format!("  {}\n", primary_location(primary)));
                if let Some(code) = &primary.code {
                    output.push_str(&format!("  {code}\n"));
                }
                output.push_str(&format!("  {}\n", primary.message));
            }
            None => output.push_str("  none detected\n"),
        }

        output.push_str("\nSuggested context\n");
        if self.context_items.is_empty() {
            output.push_str("  none\n");
        } else {
            for item in &self.context_items {
                output.push_str(&format!("  {}\n", item.target));
                output.push_str(&format!("  reason: {}\n", item.reason));
            }
        }

        output
    }
}

/// Format a primary diagnostic's location as `file:line:col`, omitting parts
/// that are absent.
pub(crate) fn primary_location(primary: &PrimaryDiagnostic) -> String {
    match (primary.file.as_deref(), primary.line, primary.column) {
        (Some(file), Some(line), Some(column)) => format!("{file}:{line}:{column}"),
        (Some(file), Some(line), None) => format!("{file}:{line}"),
        (Some(file), None, _) => file.to_string(),
        _ => "location unavailable".to_string(),
    }
}

/// Build an [`EvidencePacket`] from a compacted run and its raw output.
pub fn build(
    run_id: &str,
    exit_code: i32,
    compact: &CompactPacket,
    stdout: &str,
    stderr: &str,
) -> EvidencePacket {
    let diagnostics = extract::extract_diagnostics(stdout, stderr, exit_code);
    let file_references = collect_file_references(stdout, stderr);

    let likely_failure = derive_likely_failure(&diagnostics, exit_code);
    let primary_diagnostic = derive_primary_diagnostic(&diagnostics, &file_references);
    let context_items = derive_context_items(primary_diagnostic.as_ref(), likely_failure.as_ref());

    EvidencePacket {
        schema_version: 1,
        run_id: run_id.to_string(),
        outcome: Outcome {
            exit_code,
            status: status_label(exit_code).to_string(),
        },
        likely_failure,
        primary_diagnostic,
        diagnostics: diagnostics.iter().map(evidence_diagnostic).collect(),
        file_refs: file_references.iter().map(file_ref).collect(),
        context_items,
        token_summary: token_summary(compact),
    }
}

fn collect_file_references(stdout: &str, stderr: &str) -> Vec<FileReference> {
    let mut references = extract::extract_file_references(stderr, "stderr");
    for reference in extract::extract_file_references(stdout, "stdout") {
        if !references
            .iter()
            .any(|existing| existing.path == reference.path && existing.line == reference.line)
        {
            references.push(reference);
        }
    }
    references
}

fn derive_likely_failure(diagnostics: &[Diagnostic], exit_code: i32) -> Option<LikelyFailure> {
    // 1. First high-confidence compile error.
    if let Some(diagnostic) = diagnostics
        .iter()
        .find(|d| d.kind == DiagnosticKind::RustCompileError && d.severity == Severity::Error)
    {
        return Some(LikelyFailure {
            kind: "compile_error".to_string(),
            summary: capitalize_first(&diagnostic.message),
            confidence: "high".to_string(),
        });
    }

    // 2. First failed test (or a panic standing in for one).
    if let Some(diagnostic) = diagnostics
        .iter()
        .find(|d| matches!(d.kind, DiagnosticKind::TestFailure | DiagnosticKind::Panic))
    {
        return Some(LikelyFailure {
            kind: "test_failure".to_string(),
            summary: capitalize_first(&diagnostic.message),
            confidence: "high".to_string(),
        });
    }

    // 3. The command itself failed.
    if exit_code != 0 {
        return Some(LikelyFailure {
            kind: "command_failed".to_string(),
            summary: format!("Command exited with status {exit_code}"),
            confidence: "low".to_string(),
        });
    }

    None
}

fn derive_primary_diagnostic(
    diagnostics: &[Diagnostic],
    file_references: &[FileReference],
) -> Option<PrimaryDiagnostic> {
    // 1. Error with file + line + column.
    if let Some(diagnostic) = diagnostics.iter().find(|d| {
        d.severity == Severity::Error && d.file.is_some() && d.line.is_some() && d.column.is_some()
    }) {
        return Some(primary_from_diagnostic(diagnostic));
    }

    // 2. Error with file + line.
    if let Some(diagnostic) = diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error && d.file.is_some() && d.line.is_some())
    {
        return Some(primary_from_diagnostic(diagnostic));
    }

    // 3. Failed test with a panic location.
    if let Some(diagnostic) = diagnostics.iter().find(|d| {
        matches!(d.kind, DiagnosticKind::Panic | DiagnosticKind::TestFailure)
            && d.file.is_some()
            && d.line.is_some()
    }) {
        return Some(primary_from_diagnostic(diagnostic));
    }

    // 4. First extracted file reference.
    file_references.first().map(|reference| PrimaryDiagnostic {
        source: reference.source.clone(),
        message: reference.reason.to_string(),
        code: None,
        file: Some(reference.path.clone()),
        line: Some(reference.line),
        column: reference.column,
    })
}

fn derive_context_items(
    primary: Option<&PrimaryDiagnostic>,
    likely_failure: Option<&LikelyFailure>,
) -> Vec<ContextItem> {
    let Some(primary) = primary else {
        return Vec::new();
    };
    let (Some(file), Some(line)) = (primary.file.as_deref(), primary.line) else {
        return Vec::new();
    };

    let start = line.saturating_sub(CONTEXT_RADIUS).max(1);
    let end = line.saturating_add(CONTEXT_RADIUS);

    vec![ContextItem {
        kind: "file_window".to_string(),
        target: format!("{file}:{start}-{end}"),
        reason: context_reason(likely_failure).to_string(),
    }]
}

/// Describe why the primary location is worth inspecting, tailored to the kind
/// of failure so the reason stays accurate (not every primary is a compiler
/// diagnostic).
fn context_reason(likely_failure: Option<&LikelyFailure>) -> &'static str {
    match likely_failure.map(|failure| failure.kind.as_str()) {
        Some("compile_error") => "Primary compiler diagnostic location",
        Some("test_failure") => "Primary test failure location",
        _ => "Primary failure location",
    }
}

fn primary_from_diagnostic(diagnostic: &Diagnostic) -> PrimaryDiagnostic {
    PrimaryDiagnostic {
        source: diagnostic.source.clone(),
        message: diagnostic.message.clone(),
        code: diagnostic.code.clone(),
        file: diagnostic.file.clone(),
        line: diagnostic.line,
        column: diagnostic.column,
    }
}

fn evidence_diagnostic(diagnostic: &Diagnostic) -> EvidenceDiagnostic {
    EvidenceDiagnostic {
        kind: diagnostic.kind,
        severity: diagnostic.severity,
        code: diagnostic.code.clone(),
        message: diagnostic.message.clone(),
        file: diagnostic.file.clone(),
        line: diagnostic.line,
        column: diagnostic.column,
    }
}

fn file_ref(reference: &FileReference) -> FileRef {
    FileRef {
        file: reference.path.clone(),
        line: reference.line,
        column: reference.column,
        reason: reference.reason.to_string(),
    }
}

fn token_summary(compact: &CompactPacket) -> TokenSummary {
    let raw_tokens = compact.raw_tokens;
    let packet_tokens = compact.packet_tokens;
    let saved_tokens = raw_tokens.saturating_sub(packet_tokens);

    TokenSummary {
        raw_tokens,
        packet_tokens,
        saved_tokens,
        reduction_percent: reduction_percent(raw_tokens, saved_tokens),
    }
}

fn reduction_percent(raw_tokens: usize, saved_tokens: usize) -> f64 {
    if raw_tokens == 0 {
        return 0.0;
    }

    let percent = saved_tokens as f64 / raw_tokens as f64 * 100.0;
    (percent * 10.0).round() / 10.0
}

fn status_label(exit_code: i32) -> &'static str {
    if exit_code == 0 { "success" } else { "failure" }
}

fn capitalize_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::*;
    use crate::compactor::{CompactionInput, NativeHeuristicCompactor, OutputCompactor};

    fn compile_error_stderr() -> &'static str {
        "error[E0063]: missing field `model` in initializer of `config::Config`\n  --> src/config.rs:54:9"
    }

    fn native_compact(stderr: &str) -> CompactPacket {
        let stdout_artifact = PathBuf::from("stdout.txt");
        let stderr_artifact = PathBuf::from("stderr.txt");
        let input = CompactionInput {
            command: "cargo",
            args: &["test".to_string()],
            exit_code: 101,
            duration: Duration::from_millis(0),
            stdout: b"",
            stderr: stderr.as_bytes(),
            stdout_artifact: &stdout_artifact,
            stderr_artifact: &stderr_artifact,
        };
        NativeHeuristicCompactor
            .compact(&input)
            .expect("native compaction should succeed")
    }

    fn packet_with_tokens(raw: usize, packet: usize) -> CompactPacket {
        CompactPacket {
            compactor: "native".to_string(),
            rtk_version: None,
            command: "cargo test".to_string(),
            exit_code: 101,
            duration_ms: 0,
            failed: true,
            stdout_artifact: "stdout.txt".to_string(),
            stderr_artifact: "stderr.txt".to_string(),
            compact_artifact: None,
            raw_stdout_tokens: 0,
            raw_stderr_tokens: raw,
            raw_tokens: raw,
            packet_tokens: packet,
            preserved_items: Vec::new(),
            omitted_items: Vec::new(),
            notes: Vec::new(),
            compact_text: None,
        }
    }

    #[test]
    fn derives_compile_error_evidence() {
        let compact = packet_with_tokens(93, 50);
        let evidence = build("run-1", 101, &compact, "", compile_error_stderr());

        let likely = evidence.likely_failure.expect("likely failure");
        assert_eq!(likely.kind, "compile_error");
        assert_eq!(
            likely.summary,
            "Missing field `model` in initializer of `config::Config`"
        );
        assert_eq!(likely.confidence, "high");

        let primary = evidence.primary_diagnostic.expect("primary diagnostic");
        assert_eq!(primary.code.as_deref(), Some("E0063"));
        assert_eq!(primary.file.as_deref(), Some("src/config.rs"));
        assert_eq!(primary.line, Some(54));
        assert_eq!(primary.column, Some(9));

        assert_eq!(evidence.context_items.len(), 1);
        assert_eq!(evidence.context_items[0].target, "src/config.rs:44-64");

        assert_eq!(evidence.token_summary.raw_tokens, 93);
        assert_eq!(evidence.token_summary.packet_tokens, 50);
        assert_eq!(evidence.token_summary.saved_tokens, 43);
        assert_eq!(evidence.token_summary.reduction_percent, 46.2);
    }

    #[test]
    fn command_failure_without_diagnostics_still_reports() {
        let compact = packet_with_tokens(10, 10);
        let evidence = build("run-2", 1, &compact, "", "");

        let likely = evidence.likely_failure.expect("likely failure");
        assert_eq!(likely.kind, "command_failed");
        assert_eq!(evidence.outcome.status, "failure");
    }

    #[test]
    fn success_has_no_likely_failure() {
        let compact = packet_with_tokens(10, 5);
        let evidence = build("run-3", 0, &compact, "", "");

        assert!(evidence.likely_failure.is_none());
        assert_eq!(evidence.outcome.status, "success");
    }

    #[test]
    fn evidence_snapshot_for_compile_error_fixture() {
        let stderr = include_str!("../fixtures/outputs/rust_compile_error.stderr");
        let compact = native_compact(stderr);
        let evidence = build("2026-07-08T082655Z-13e58c", 101, &compact, "", stderr);

        insta::assert_json_snapshot!("evidence_compile_error", evidence);
    }

    #[test]
    fn evidence_snapshot_for_test_failure_fixture() {
        let stderr = include_str!("../fixtures/outputs/rust_test_failure.stderr");
        let compact = native_compact(stderr);
        let evidence = build("2026-07-08T082655Z-13e58c", 101, &compact, "", stderr);

        insta::assert_json_snapshot!("evidence_test_failure", evidence);
    }
}
