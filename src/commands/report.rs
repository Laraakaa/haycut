use std::{io, path::Path};

use crate::{
    budget::{BudgetStatus, BudgetUsage},
    code_context::{CodeContext, render_code_context},
    commands::{
        read_symbol::{self, SymbolMatch},
        run_context::RunContext,
        trace::RunManifest,
    },
    config::{Config, TokenConfig},
    evidence::{
        self, ContextItem, EvidenceDiagnostic, EvidencePacket, FileRef, LikelyFailure,
        PrimaryDiagnostic,
    },
    store::RUN_STORE_PATH,
    util::format_count,
};
use serde::Serialize;

pub fn run(json: bool, markdown: bool, symbols: Vec<String>) -> i32 {
    let format = match ReportFormat::from_flags(json, markdown) {
        Ok(format) => format,
        Err(error) => {
            eprintln!("Error: {error}");
            return 2;
        }
    };

    let config = match Config::load_from_current_dir() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Error loading config: {error}");
            return 1;
        }
    };

    match load_last_report(Path::new(RUN_STORE_PATH), &symbols) {
        Ok(report) => {
            if let Err(error) = print_report(&report, &config.token, format) {
                eprintln!("Error rendering report: {error}");
                return 1;
            }
            0
        }
        Err(error) => {
            eprintln!("Error loading report: {error}");
            1
        }
    }
}

/// A report is a pure view over a stored run's evidence packet plus optional
/// symbol snippets. All failure intelligence lives in [`crate::evidence`];
/// this module only renders it.
#[derive(Debug)]
struct Report {
    manifest: RunManifest,
    evidence: EvidencePacket,
    symbols: Vec<SymbolMatch>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReportFormat {
    Text,
    Json,
    Markdown,
}

impl ReportFormat {
    fn from_flags(json: bool, markdown: bool) -> Result<Self, &'static str> {
        match (json, markdown) {
            (true, true) => Err("report accepts only one output format: --json or --markdown"),
            (true, false) => Ok(Self::Json),
            (false, true) => Ok(Self::Markdown),
            (false, false) => Ok(Self::Text),
        }
    }
}

fn load_last_report(root: &Path, symbol_targets: &[String]) -> io::Result<Report> {
    let ctx = RunContext::load_last(root)?;
    let symbols = load_symbols(Path::new(&ctx.manifest.cwd), symbol_targets)?;
    Ok(Report {
        manifest: ctx.manifest,
        evidence: ctx.evidence,
        symbols,
    })
}

fn print_report(
    report: &Report,
    token_config: &TokenConfig,
    format: ReportFormat,
) -> io::Result<()> {
    match format {
        ReportFormat::Json => {
            let report = json_report(report, token_config);
            let rendered = serde_json::to_string_pretty(&report).map_err(io::Error::other)?;
            println!("{rendered}");
        }
        ReportFormat::Markdown => print!("{}", markdown_report(report, token_config)),
        ReportFormat::Text => print!("{}", human_report(report, token_config)),
    }

    Ok(())
}

/// Token totals for the report, folding symbol snippets into the packet cost.
struct TokenView {
    raw: usize,
    packet: usize,
    saved: usize,
    reduction: f64,
}

fn token_view(report: &Report) -> TokenView {
    let raw = report.evidence.token_summary.raw_tokens;
    let symbol_tokens: usize = report.symbols.iter().map(|s| s.estimated_tokens).sum();
    let packet = report.evidence.token_summary.packet_tokens + symbol_tokens;
    let saved = raw.saturating_sub(packet);

    TokenView {
        raw,
        packet,
        saved,
        reduction: reduction_percent(raw, packet),
    }
}

// ── JSON ──────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct JsonReport<'a> {
    schema_version: u8,
    run: JsonRun,
    outcome: JsonOutcome<'a>,
    token_summary: JsonTokenSummary,
    budget: JsonBudget,
    #[serde(skip_serializing_if = "Option::is_none")]
    likely_failure: &'a Option<LikelyFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_diagnostic: &'a Option<PrimaryDiagnostic>,
    diagnostics: &'a [EvidenceDiagnostic],
    file_refs: &'a [FileRef],
    context_items: &'a [ContextItem],
    artefacts: JsonArtefacts,
    symbols: Vec<JsonSymbol>,
}

#[derive(Serialize)]
struct JsonRun {
    id: String,
    command: String,
    duration_ms: u128,
    created_at: String,
}

#[derive(Serialize)]
struct JsonOutcome<'a> {
    exit_code: i32,
    status: &'a str,
}

#[derive(Serialize)]
struct JsonTokenSummary {
    raw_tokens: usize,
    raw_stdout_tokens: usize,
    raw_stderr_tokens: usize,
    packet_tokens: usize,
    saved_tokens: usize,
    reduction_percent: f64,
}

#[derive(Serialize)]
struct JsonBudget {
    soft: usize,
    hard: usize,
    status: &'static str,
}

#[derive(Serialize)]
struct JsonArtefacts {
    run_manifest: String,
    stdout: String,
    stderr: String,
    compact: String,
    evidence: String,
}

#[derive(Serialize)]
struct JsonSymbol {
    path: String,
    name: String,
    start_line: usize,
    end_line: usize,
    estimated_tokens: usize,
}

fn json_report<'a>(report: &'a Report, token_config: &TokenConfig) -> JsonReport<'a> {
    let tokens = token_view(report);
    let budget = BudgetUsage::from_config(token_config, tokens.raw, tokens.packet);

    JsonReport {
        schema_version: 1,
        run: JsonRun {
            id: report.manifest.id.clone(),
            command: report.manifest.command.clone(),
            duration_ms: report.manifest.duration_ms,
            created_at: report.manifest.created_at.to_rfc3339(),
        },
        outcome: JsonOutcome {
            exit_code: report.evidence.outcome.exit_code,
            status: &report.evidence.outcome.status,
        },
        token_summary: JsonTokenSummary {
            raw_tokens: tokens.raw,
            raw_stdout_tokens: report.manifest.raw_stdout_tokens_estimated,
            raw_stderr_tokens: report.manifest.raw_stderr_tokens_estimated,
            packet_tokens: tokens.packet,
            saved_tokens: tokens.saved,
            reduction_percent: tokens.reduction,
        },
        budget: JsonBudget {
            soft: budget.soft_budget,
            hard: budget.hard_budget,
            status: budget_status_label(budget.status),
        },
        likely_failure: &report.evidence.likely_failure,
        primary_diagnostic: &report.evidence.primary_diagnostic,
        diagnostics: &report.evidence.diagnostics,
        file_refs: &report.evidence.file_refs,
        context_items: &report.evidence.context_items,
        artefacts: json_artefacts(report),
        symbols: report
            .symbols
            .iter()
            .map(|symbol| JsonSymbol {
                path: symbol.path.clone(),
                name: symbol.symbol.name.clone(),
                start_line: symbol.symbol.start_line,
                end_line: symbol.symbol.end_line,
                estimated_tokens: symbol.estimated_tokens,
            })
            .collect(),
    }
}

fn json_artefacts(report: &Report) -> JsonArtefacts {
    JsonArtefacts {
        run_manifest: artefact_path(report, "run.json"),
        stdout: artefact_path(report, &report.manifest.stdout),
        stderr: artefact_path(report, &report.manifest.stderr),
        compact: artefact_path(report, &report.manifest.compact),
        evidence: artefact_path(report, &report.manifest.evidence),
    }
}

// ── human text ────────────────────────────────────────────────────────────────

fn human_report(report: &Report, token_config: &TokenConfig) -> String {
    let tokens = token_view(report);
    let budget = BudgetUsage::from_config(token_config, tokens.raw, tokens.packet);
    let mut output = String::new();

    output.push_str("HayCut report\n\n");

    output.push_str("Result\n");
    output.push_str(&format!("  run: {}\n", report.manifest.id));
    output.push_str(&format!("  command: {}\n", report.manifest.command));
    output.push_str(&format!(
        "  exit code: {}\n",
        report.evidence.outcome.exit_code
    ));
    output.push_str(&format!("  status: {}\n", report.evidence.outcome.status));
    output.push_str(&format!(
        "  duration: {}\n",
        format_duration(report.manifest.duration_ms)
    ));

    output.push_str("\nToken spend\n");
    output.push_str(&format!("  raw: {}\n", format_count(tokens.raw)));
    output.push_str(&format!("  packet: {}\n", format_count(tokens.packet)));
    output.push_str(&format!("  saved: {}\n", format_count(tokens.saved)));
    output.push_str(&format!("  reduction: {:.1}%\n", tokens.reduction));
    output.push_str(&format!(
        "  budget: {} (soft {}, hard {})\n",
        budget_status_label(budget.status),
        format_count(budget.soft_budget),
        format_count(budget.hard_budget)
    ));

    output.push_str("\nLikely failure\n");
    match &report.evidence.likely_failure {
        Some(failure) => {
            output.push_str(&format!("  {}: {}\n", failure.kind, failure.summary));
        }
        None => output.push_str("  none detected\n"),
    }

    output.push_str("\nPrimary diagnostic\n");
    match &report.evidence.primary_diagnostic {
        Some(primary) => {
            output.push_str(&format!("  {}\n", evidence::primary_location(primary)));
            output.push_str(&format!("  {}\n", primary_detail(primary)));
        }
        None => output.push_str("  none detected\n"),
    }

    output.push_str("\nContext candidates\n");
    if report.evidence.context_items.is_empty() {
        output.push_str("  none\n");
    } else {
        for item in &report.evidence.context_items {
            output.push_str(&format!("  {}\n", item.target));
            output.push_str(&format!("    reason: {}\n", item.reason));
        }
    }

    if !report.symbols.is_empty() {
        output.push_str("\nSymbols\n");
        for symbol in &report.symbols {
            output.push_str(&render_code_context(CodeContext {
                symbol: Some(&symbol.symbol.name),
                path: Some(&symbol.path),
                start_line: Some(symbol.symbol.start_line),
                source: &symbol.code,
                semantic_label: None,
            }));
            output.push_str(&format!(
                "estimated tokens: {}\n",
                format_count(symbol.estimated_tokens)
            ));
        }
    }

    output.push_str("\nArtefacts\n");
    output.push_str(&format!(
        "  run metadata: {}\n",
        artefact_path(report, "run.json")
    ));
    output.push_str(&format!(
        "  compact packet: {}\n",
        artefact_path(report, &report.manifest.compact)
    ));
    output.push_str(&format!(
        "  evidence packet: {}\n",
        artefact_path(report, &report.manifest.evidence)
    ));
    output.push_str(&format!(
        "  stdout: {}\n",
        artefact_path(report, &report.manifest.stdout)
    ));
    output.push_str(&format!(
        "  stderr: {}\n",
        artefact_path(report, &report.manifest.stderr)
    ));

    output
}

// ── markdown ──────────────────────────────────────────────────────────────────

fn markdown_report(report: &Report, token_config: &TokenConfig) -> String {
    let tokens = token_view(report);
    let budget = BudgetUsage::from_config(token_config, tokens.raw, tokens.packet);
    let mut output = String::new();

    output.push_str("# HayCut Report\n\n");

    output.push_str("## Result\n\n");
    output.push_str(&format!("- **Run:** `{}`\n", report.manifest.id));
    output.push_str(&format!(
        "- **Command:** {}\n",
        markdown_inline_code(&report.manifest.command)
    ));
    output.push_str(&format!(
        "- **Status:** {} (exit `{}`)\n",
        report.evidence.outcome.status, report.evidence.outcome.exit_code
    ));
    output.push_str(&format!(
        "- **Duration:** `{}`\n",
        format_duration(report.manifest.duration_ms)
    ));

    output.push_str("\n## Token spend\n\n");
    output.push_str("| Metric | Estimate |\n");
    output.push_str("| --- | ---: |\n");
    output.push_str(&format!("| Raw | {} |\n", format_count(tokens.raw)));
    output.push_str(&format!("| Packet | {} |\n", format_count(tokens.packet)));
    output.push_str(&format!("| Saved | {} |\n", format_count(tokens.saved)));
    output.push_str(&format!("| Reduction | {:.1}% |\n", tokens.reduction));
    output.push_str(&format!(
        "| Budget | {} |\n",
        budget_status_label(budget.status)
    ));

    output.push_str("\n## Likely failure\n\n");
    match &report.evidence.likely_failure {
        Some(failure) => output.push_str(&format!(
            "- **{}:** {}\n",
            failure.kind,
            markdown_inline_code(&failure.summary)
        )),
        None => output.push_str("- None detected.\n"),
    }

    output.push_str("\n## Primary diagnostic\n\n");
    match &report.evidence.primary_diagnostic {
        Some(primary) => {
            output.push_str(&format!(
                "- **Location:** {}\n",
                markdown_inline_code(&evidence::primary_location(primary))
            ));
            output.push_str(&format!(
                "- **Detail:** {}\n",
                markdown_inline_code(&primary_detail(primary))
            ));
        }
        None => output.push_str("- None detected.\n"),
    }

    output.push_str("\n## Context candidates\n\n");
    if report.evidence.context_items.is_empty() {
        output.push_str("- None.\n");
    } else {
        for item in &report.evidence.context_items {
            output.push_str(&format!(
                "- {} — {}\n",
                markdown_inline_code(&item.target),
                item.reason
            ));
        }
    }

    if !report.symbols.is_empty() {
        output.push_str("\n## Symbols\n\n");
        for symbol in &report.symbols {
            output.push_str(&format!(
                "- {} lines {}-{} ({} tokens)\n",
                markdown_inline_code(&format!("{}::{}", symbol.path, symbol.symbol.name)),
                symbol.symbol.start_line,
                symbol.symbol.end_line,
                format_count(symbol.estimated_tokens)
            ));
        }
    }

    output.push_str("\n## Artefacts\n\n");
    output.push_str(&format!(
        "- [run metadata]({})\n",
        markdown_link_target(&artefact_path(report, "run.json"))
    ));
    output.push_str(&format!(
        "- [compact packet]({})\n",
        markdown_link_target(&artefact_path(report, &report.manifest.compact))
    ));
    output.push_str(&format!(
        "- [evidence packet]({})\n",
        markdown_link_target(&artefact_path(report, &report.manifest.evidence))
    ));
    output.push_str(&format!(
        "- [stdout]({})\n",
        markdown_link_target(&artefact_path(report, &report.manifest.stdout))
    ));
    output.push_str(&format!(
        "- [stderr]({})\n",
        markdown_link_target(&artefact_path(report, &report.manifest.stderr))
    ));

    output
}

// ── shared helpers ────────────────────────────────────────────────────────────

/// A one-line detail for the primary diagnostic: `error[CODE]` when a code is
/// present, otherwise the diagnostic message.
fn primary_detail(primary: &PrimaryDiagnostic) -> String {
    match &primary.code {
        Some(code) => format!("error[{code}]"),
        None => primary.message.clone(),
    }
}

fn artefact_path(_report: &Report, file: &str) -> String {
    match file {
        "run.json" => "sqlite:runs".to_string(),
        "sqlite:compact_json" | "sqlite:evidence_json" => file.to_string(),
        _ => report_path(file),
    }
}

fn report_path(path: &str) -> String {
    path.to_string()
}

fn markdown_inline_code(value: &str) -> String {
    format!("`{}`", value.replace('`', "'"))
}

fn markdown_link_target(path: &str) -> String {
    path.replace(' ', "%20")
}

fn load_symbols(root: &Path, symbol_targets: &[String]) -> io::Result<Vec<SymbolMatch>> {
    symbol_targets
        .iter()
        .map(|target| read_symbol::read_symbol(root, target))
        .collect()
}

fn reduction_percent(raw_tokens: usize, packet_tokens: usize) -> f64 {
    if raw_tokens == 0 {
        return 0.0;
    }

    let reduced_tokens = raw_tokens.saturating_sub(packet_tokens);
    reduced_tokens as f64 / raw_tokens as f64 * 100.0
}

fn format_duration(duration_ms: u128) -> String {
    format!("{:.2}s", duration_ms as f64 / 1000.0)
}

fn budget_status_label(status: BudgetStatus) -> &'static str {
    match status {
        BudgetStatus::Within => "within budget",
        BudgetStatus::SoftExceeded => "over soft budget",
        BudgetStatus::HardExceeded => "over hard budget",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env, fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use chrono::Utc;

    use super::*;
    use crate::compactor::CompactPacket;
    use crate::evidence::{Outcome, TokenSummary};
    use crate::extract::{DiagnosticKind, Severity};
    use crate::store::{NewArtifact, NewRun, insert_run};
    use crate::symbols::{Symbol, SymbolKind};

    #[test]
    fn loads_last_report_from_sqlite() {
        let root = temp_run_root("sqlite-last-run");
        let older = root.join("older");
        let latest = root.join("latest");
        let db_path = root.join("haycut.sqlite3");
        write_report_artifacts(&older, "older", "older command", 20)
            .expect("older artifacts should be written");
        write_report_artifacts(&latest, "newer", "newer command", 10)
            .expect("newer artifacts should be written");
        insert_report_run(
            &db_path,
            "older",
            "older command",
            20,
            "2026-07-07T15:29:00+00:00",
            &older,
        )
        .expect("older run should insert");
        insert_report_run(
            &db_path,
            "newer",
            "newer command",
            10,
            "2026-07-07T15:30:00+00:00",
            &latest,
        )
        .expect("newer run should insert");

        let report = load_last_report(&db_path, &[]).expect("last report should load from SQLite");

        assert_eq!(report.manifest.id, "newer");
        assert_eq!(report.manifest.command, "newer command");
        assert_eq!(report.evidence.token_summary.packet_tokens, 10);

        fs::remove_dir_all(root).expect("test run root should be removed");
    }

    #[test]
    fn calculates_reduction_percentage() {
        assert_eq!(reduction_percent(0, 10), 0.0);
        assert_eq!(reduction_percent(100, 25), 75.0);
        assert_eq!(reduction_percent(100, 125), 0.0);
    }

    #[test]
    fn rejects_multiple_report_output_formats() {
        assert_eq!(
            ReportFormat::from_flags(false, false),
            Ok(ReportFormat::Text)
        );
        assert_eq!(
            ReportFormat::from_flags(true, false),
            Ok(ReportFormat::Json)
        );
        assert_eq!(
            ReportFormat::from_flags(false, true),
            Ok(ReportFormat::Markdown)
        );
        assert_eq!(
            ReportFormat::from_flags(true, true),
            Err("report accepts only one output format: --json or --markdown")
        );
    }

    #[test]
    fn human_report_renders_evidence_sections() {
        let report = report_fixture();

        let rendered = human_report(&report, &token_config());

        assert!(rendered.contains("HayCut report"));
        assert!(rendered.contains("Result\n"));
        assert!(rendered.contains("  command: cargo test"));
        assert!(rendered.contains("  exit code: 101"));
        assert!(rendered.contains("  status: failure"));
        assert!(rendered.contains("Token spend\n"));
        assert!(rendered.contains("  raw: 93"));
        assert!(rendered.contains("  packet: 50"));
        assert!(rendered.contains("  saved: 43"));
        assert!(rendered.contains("  reduction: 46.2%"));
        assert!(rendered.contains("Likely failure\n"));
        assert!(
            rendered.contains(
                "  compile_error: Missing field `model` in initializer of `config::Config`"
            )
        );
        assert!(rendered.contains("Primary diagnostic\n"));
        assert!(rendered.contains("  src/config.rs:54:9"));
        assert!(rendered.contains("  error[E0063]"));
        assert!(rendered.contains("Context candidates\n"));
        assert!(rendered.contains("  src/config.rs:44-64"));
        assert!(rendered.contains("    reason: Primary compiler diagnostic location"));
        assert!(rendered.contains("Artefacts\n"));
        assert!(rendered.contains("evidence packet: sqlite:evidence_json"));
    }

    #[test]
    fn human_report_reports_no_failure_for_success() {
        let mut report = report_fixture();
        report.evidence.outcome = Outcome {
            exit_code: 0,
            status: "success".to_string(),
        };
        report.evidence.likely_failure = None;
        report.evidence.primary_diagnostic = None;
        report.evidence.context_items = Vec::new();

        let rendered = human_report(&report, &token_config());

        assert!(rendered.contains("  status: success"));
        assert!(rendered.contains("Likely failure\n  none detected"));
        assert!(rendered.contains("Primary diagnostic\n  none detected"));
        assert!(rendered.contains("Context candidates\n  none"));
    }

    #[test]
    fn human_report_includes_symbol_snippets() {
        let mut report = report_fixture();
        report.symbols.push(SymbolMatch {
            path: "src/auth/session.rs".to_string(),
            symbol: Symbol {
                kind: SymbolKind::Function,
                name: "validate_session".to_string(),
                start_line: 88,
                end_line: 90,
                start_byte: 0,
                end_byte: 0,
            },
            code: "fn validate_session() -> bool {\n    false\n}".to_string(),
            estimated_tokens: 12,
        });

        let rendered = human_report(&report, &token_config());

        assert!(rendered.contains("validate_session@src/auth/session.rs:88\n```rust\n"));
        assert!(rendered.contains("fn validate_session() -> bool"));
        // Symbol tokens fold into packet spend: 50 + 12 = 62.
        assert!(rendered.contains("  packet: 62"));
    }

    #[test]
    fn json_report_exposes_evidence_and_hides_raw_output() {
        let report = report_fixture();
        let rendered = serde_json::to_string(&json_report(&report, &token_config()))
            .expect("JSON report should serialize");
        let value: serde_json::Value =
            serde_json::from_str(&rendered).expect("JSON report should parse");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["run"]["id"], "run-1");
        assert_eq!(value["outcome"]["exit_code"], 101);
        assert_eq!(value["outcome"]["status"], "failure");
        assert_eq!(value["token_summary"]["raw_tokens"], 93);
        assert_eq!(value["token_summary"]["packet_tokens"], 50);
        assert_eq!(value["token_summary"]["saved_tokens"], 43);
        assert_eq!(value["likely_failure"]["kind"], "compile_error");
        assert_eq!(value["primary_diagnostic"]["code"], "E0063");
        assert_eq!(value["primary_diagnostic"]["file"], "src/config.rs");
        assert_eq!(value["context_items"][0]["target"], "src/config.rs:44-64");
        assert_eq!(value["artefacts"]["evidence"], "sqlite:evidence_json");
        assert!(!rendered.contains("preserved_items"));
    }

    #[test]
    fn markdown_report_renders_evidence_sections() {
        let report = report_fixture();

        let rendered = markdown_report(&report, &token_config());

        assert!(rendered.starts_with("# HayCut Report"));
        assert!(rendered.contains("## Result"));
        assert!(rendered.contains("- **Command:** `cargo test`"));
        assert!(rendered.contains("- **Status:** failure (exit `101`)"));
        assert!(rendered.contains("## Token spend"));
        assert!(rendered.contains("| Raw | 93 |"));
        assert!(rendered.contains("| Packet | 50 |"));
        assert!(rendered.contains("## Likely failure"));
        assert!(rendered.contains("- **compile_error:**"));
        assert!(rendered.contains("## Primary diagnostic"));
        assert!(rendered.contains("`src/config.rs:54:9`"));
        assert!(rendered.contains("`error[E0063]`"));
        assert!(rendered.contains("## Context candidates"));
        assert!(rendered.contains("`src/config.rs:44-64` — Primary compiler diagnostic location"));
        assert!(rendered.contains("## Artefacts"));
        assert!(rendered.contains("- [evidence packet](sqlite:evidence_json)"));
    }

    fn report_fixture() -> Report {
        Report {
            manifest: manifest_fixture(),
            evidence: evidence_fixture(),
            symbols: Vec::new(),
        }
    }

    fn manifest_fixture() -> RunManifest {
        RunManifest {
            id: "run-1".to_string(),
            command: "cargo test".to_string(),
            args: vec!["test".to_string()],
            cwd: "/tmp".to_string(),
            exit_code: 101,
            duration_ms: 2_260,
            stdout_bytes: 0,
            stderr_bytes: 0,
            estimated_raw_tokens: 93,
            raw_stdout_tokens_estimated: 0,
            raw_stderr_tokens_estimated: 93,
            created_at: Utc::now(),
            stdout: ".haycut/runs/run-1/stdout.txt".to_string(),
            stderr: ".haycut/runs/run-1/stderr.txt".to_string(),
            compact: "sqlite:compact_json".to_string(),
            evidence: "sqlite:evidence_json".to_string(),
        }
    }

    fn evidence_fixture() -> EvidencePacket {
        EvidencePacket {
            schema_version: 1,
            run_id: "run-1".to_string(),
            outcome: Outcome {
                exit_code: 101,
                status: "failure".to_string(),
            },
            likely_failure: Some(LikelyFailure {
                kind: "compile_error".to_string(),
                summary: "Missing field `model` in initializer of `config::Config`".to_string(),
                confidence: "high".to_string(),
            }),
            primary_diagnostic: Some(PrimaryDiagnostic {
                source: "stderr".to_string(),
                message: "missing field `model` in initializer of `config::Config`".to_string(),
                code: Some("E0063".to_string()),
                file: Some("src/config.rs".to_string()),
                line: Some(54),
                column: Some(9),
            }),
            diagnostics: vec![EvidenceDiagnostic {
                kind: DiagnosticKind::RustCompileError,
                severity: Severity::Error,
                code: Some("E0063".to_string()),
                message: "missing field `model` in initializer of `config::Config`".to_string(),
                file: Some("src/config.rs".to_string()),
                line: Some(54),
                column: Some(9),
            }],
            file_refs: vec![FileRef {
                file: "src/config.rs".to_string(),
                line: 54,
                column: Some(9),
                reason: "compiler diagnostic location".to_string(),
            }],
            context_items: vec![ContextItem {
                kind: "file_window".to_string(),
                target: "src/config.rs:44-64".to_string(),
                reason: "Primary compiler diagnostic location".to_string(),
            }],
            token_summary: TokenSummary {
                raw_tokens: 93,
                packet_tokens: 50,
                saved_tokens: 43,
                reduction_percent: 46.2,
            },
        }
    }

    fn token_config() -> TokenConfig {
        TokenConfig {
            soft_budget: 40_000,
            hard_budget: 80_000,
        }
    }

    fn temp_run_root(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "haycut-report-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos()
        ))
    }

    fn write_report_artifacts(
        run_directory: &Path,
        id: &str,
        command: &str,
        packet_tokens: usize,
    ) -> io::Result<()> {
        fs::create_dir_all(run_directory)?;
        let _ = (id, command, packet_tokens);
        fs::write(run_directory.join("stdout.txt"), "")?;
        fs::write(run_directory.join("stderr.txt"), "")
    }

    fn compact_fixture(command: &str, packet_tokens: usize) -> CompactPacket {
        CompactPacket {
            compactor: "native".to_string(),
            rtk_version: None,
            command: command.to_string(),
            exit_code: 0,
            duration_ms: 42,
            failed: false,
            stdout_artifact: "stdout.txt".to_string(),
            stderr_artifact: "stderr.txt".to_string(),
            compact_artifact: None,
            raw_stdout_tokens: 80,
            raw_stderr_tokens: 20,
            raw_tokens: 100,
            packet_tokens,
            preserved_items: Vec::new(),
            omitted_items: Vec::new(),
            notes: Vec::new(),
            compact_text: None,
        }
    }

    fn insert_report_run(
        db_path: &Path,
        id: &str,
        command: &str,
        packet_tokens: usize,
        created_at: &str,
        run_directory: &Path,
    ) -> io::Result<()> {
        let compact = compact_fixture(command, packet_tokens);
        let evidence = EvidencePacket {
            schema_version: 1,
            run_id: id.to_string(),
            outcome: Outcome {
                exit_code: 0,
                status: "success".to_string(),
            },
            likely_failure: None,
            primary_diagnostic: None,
            diagnostics: Vec::new(),
            file_refs: Vec::new(),
            context_items: Vec::new(),
            token_summary: TokenSummary {
                raw_tokens: 100,
                packet_tokens,
                saved_tokens: 100_usize.saturating_sub(packet_tokens),
                reduction_percent: 0.0,
            },
        };
        let compact_json = serde_json::to_string(&compact).map_err(io::Error::other)?;
        let evidence_json = serde_json::to_string(&evidence).map_err(io::Error::other)?;
        let stdout_path = run_directory.join("stdout.txt").display().to_string();
        let stderr_path = run_directory.join("stderr.txt").display().to_string();

        insert_run(
            db_path,
            &NewRun {
                id,
                command,
                args_json: "[]",
                cwd: "/tmp",
                exit_code: Some(0),
                duration_ms: 42,
                stdout_bytes: 0,
                stderr_bytes: 0,
                raw_tokens: 100,
                raw_stdout_tokens: 80,
                raw_stderr_tokens: 20,
                packet_tokens: packet_tokens as i64,
                created_at,
                stdout_path: &stdout_path,
                stderr_path: &stderr_path,
                compact_text_path: None,
                compact_json: &compact_json,
                evidence_json: &evidence_json,
                artifacts: vec![
                    NewArtifact {
                        id: format!("{id}:stdout"),
                        kind: "stdout",
                        path: stdout_path.clone(),
                        estimated_tokens: Some(80),
                    },
                    NewArtifact {
                        id: format!("{id}:stderr"),
                        kind: "stderr",
                        path: stderr_path.clone(),
                        estimated_tokens: Some(20),
                    },
                ],
            },
        )
    }
}
