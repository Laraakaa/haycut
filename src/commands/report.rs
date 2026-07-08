use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    budget::{BudgetStatus, BudgetUsage},
    commands::read_symbol::{self, SymbolMatch},
    commands::trace::RunManifest,
    compactor::{CompactPacket, OmittedItem, OutputSource, PreservedItem, PreservedKind},
    config::{Config, TokenConfig},
    store::{self, RUN_STORE_PATH},
};
use serde::Serialize;

const MAX_PRESERVED_ITEMS: usize = 8;
const MAX_OMITTED_ITEMS: usize = 8;

pub fn run(last: bool, json: bool, markdown: bool, symbols: Vec<String>) -> i32 {
    if !last {
        eprintln!("Error: report currently requires --last");
        return 2;
    }
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

#[derive(Debug)]
struct Report {
    run_directory: PathBuf,
    manifest: RunManifest,
    packet: CompactPacket,
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
    let stored_run = store::latest_run(root)?;
    let run_json_path = PathBuf::from(stored_run.artifact_path("run_manifest")?);
    let compact_path = PathBuf::from(stored_run.artifact_path("compact_json")?);
    let run_directory = run_json_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "run manifest has no parent directory",
            )
        })?;
    let manifest = RunManifest::load(&run_json_path)?;
    let compact_contents = fs::read_to_string(&compact_path)?;
    let packet = serde_json::from_str(&compact_contents).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid compact packet {}: {error}", compact_path.display()),
        )
    })?;
    let symbols = load_symbols(Path::new(&manifest.cwd), symbol_targets)?;

    Ok(Report {
        run_directory,
        manifest,
        packet,
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

#[derive(Serialize)]
struct JsonReport {
    schema_version: u8,
    run: JsonRun,
    token_estimates: JsonTokenEstimates,
    reduction_percent: f64,
    budget: JsonBudget,
    artefacts: JsonArtefacts,
    preserved_evidence_count: usize,
    omitted_evidence: Vec<JsonOmittedEvidence>,
    symbols: Vec<JsonSymbol>,
}

#[derive(Serialize)]
struct JsonRun {
    id: String,
    command: String,
    exit_code: i32,
    result: &'static str,
    duration_ms: u128,
    created_at: String,
}

#[derive(Serialize)]
struct JsonTokenEstimates {
    raw_total: usize,
    raw_stdout: usize,
    raw_stderr: usize,
    packet: usize,
    saved: usize,
}

#[derive(Serialize)]
struct JsonBudget {
    soft: usize,
    hard: usize,
    status: &'static str,
}

#[derive(Serialize)]
struct JsonArtefacts {
    stdout: String,
    stderr: String,
    compact: String,
}

#[derive(Serialize)]
struct JsonOmittedEvidence {
    source: &'static str,
    reason: String,
    count: usize,
}

#[derive(Serialize)]
struct JsonSymbol {
    path: String,
    name: String,
    start_line: usize,
    end_line: usize,
    estimated_tokens: usize,
}

fn json_report(report: &Report, token_config: &TokenConfig) -> JsonReport {
    let packet_tokens = evidence_packet_tokens(report);
    let raw_tokens_avoided = report.packet.raw_tokens.saturating_sub(packet_tokens);
    let budget = BudgetUsage::from_config(token_config, report.packet.raw_tokens, packet_tokens);
    let stdout_path = report.run_directory.join(&report.manifest.stdout);
    let stderr_path = report.run_directory.join(&report.manifest.stderr);
    let compact_path = report.run_directory.join(&report.manifest.compact);

    JsonReport {
        schema_version: 1,
        run: JsonRun {
            id: report.manifest.id.clone(),
            command: report.manifest.command.clone(),
            exit_code: report.manifest.exit_code,
            result: result_label(report.manifest.exit_code),
            duration_ms: report.manifest.duration_ms,
            created_at: report.manifest.created_at.to_rfc3339(),
        },
        token_estimates: JsonTokenEstimates {
            raw_total: report.packet.raw_tokens,
            raw_stdout: report.packet.raw_stdout_tokens,
            raw_stderr: report.packet.raw_stderr_tokens,
            packet: packet_tokens,
            saved: raw_tokens_avoided,
        },
        reduction_percent: reduction_percent(report.packet.raw_tokens, packet_tokens),
        budget: JsonBudget {
            soft: budget.soft_budget,
            hard: budget.hard_budget,
            status: budget_status_label(budget.status),
        },
        artefacts: JsonArtefacts {
            stdout: stdout_path.display().to_string(),
            stderr: stderr_path.display().to_string(),
            compact: compact_path.display().to_string(),
        },
        preserved_evidence_count: report.packet.preserved_items.len(),
        omitted_evidence: report
            .packet
            .omitted_items
            .iter()
            .filter(|item| item.count > 0)
            .map(|item| JsonOmittedEvidence {
                source: source_label(item.source),
                reason: item.reason.clone(),
                count: item.count,
            })
            .collect(),
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

fn markdown_report(report: &Report, token_config: &TokenConfig) -> String {
    let packet_tokens = evidence_packet_tokens(report);
    let raw_tokens_avoided = report.packet.raw_tokens.saturating_sub(packet_tokens);
    let budget = BudgetUsage::from_config(token_config, report.packet.raw_tokens, packet_tokens);
    let stdout_path = report.run_directory.join(&report.manifest.stdout);
    let stderr_path = report.run_directory.join(&report.manifest.stderr);
    let compact_path = report.run_directory.join(&report.manifest.compact);
    let mut output = String::new();

    output.push_str("# HayCut Report\n\n");
    output.push_str("## Result\n\n");
    output.push_str(&format!("- **Run:** `{}`\n", report.manifest.id));
    output.push_str(&format!(
        "- **Command:** {}\n",
        markdown_inline_code(&report.manifest.command)
    ));
    output.push_str(&format!(
        "- **Result:** {} (exit `{}`)\n",
        result_label(report.manifest.exit_code),
        report.manifest.exit_code
    ));
    output.push_str(&format!(
        "- **Duration:** `{}` ms\n",
        report.manifest.duration_ms
    ));
    output.push_str(&format!(
        "- **Likely failure:** {}\n",
        likely_failure(report)
            .map(markdown_inline_code)
            .unwrap_or_else(|| "none detected".to_string())
    ));

    output.push_str("\n## Token Savings\n\n");
    output.push_str("| Metric | Estimate |\n");
    output.push_str("| --- | ---: |\n");
    output.push_str(&format!(
        "| Raw tokens | {} |\n",
        format_count(report.packet.raw_tokens)
    ));
    output.push_str(&format!(
        "| Packet tokens | {} |\n",
        format_count(packet_tokens)
    ));
    output.push_str(&format!(
        "| Saved tokens | {} |\n",
        format_count(raw_tokens_avoided)
    ));
    output.push_str(&format!(
        "| Reduction | {:.1}% |\n",
        reduction_percent(report.packet.raw_tokens, packet_tokens)
    ));
    output.push_str(&format!(
        "| Budget status | {} |\n",
        budget_status_label(budget.status)
    ));

    output.push_str("\n## Evidence Summary\n\n");
    output.push_str("**Preserved**\n\n");
    for item in preserved_summary(report) {
        output.push_str(&format!("- {item}\n"));
    }
    append_markdown_preserved_items(&mut output, &report.packet.preserved_items);
    append_markdown_symbols(&mut output, report);

    output.push_str("\n**Omitted**\n\n");
    append_markdown_omitted_items(&mut output, &report.packet.omitted_items);

    output.push_str("\n## Full Artefacts\n\n");
    output.push_str(&format!(
        "- [stdout]({})\n",
        markdown_link_target(&stdout_path)
    ));
    output.push_str(&format!(
        "- [stderr]({})\n",
        markdown_link_target(&stderr_path)
    ));
    output.push_str(&format!(
        "- [compact packet]({})\n",
        markdown_link_target(&compact_path)
    ));

    output
}

fn human_report(report: &Report, token_config: &TokenConfig) -> String {
    let packet_tokens = evidence_packet_tokens(report);
    let raw_tokens_avoided = report.packet.raw_tokens.saturating_sub(packet_tokens);
    let budget = BudgetUsage::from_config(token_config, report.packet.raw_tokens, packet_tokens);
    let stdout_path = report.run_directory.join(&report.manifest.stdout);
    let stderr_path = report.run_directory.join(&report.manifest.stderr);
    let compact_path = report.run_directory.join(&report.manifest.compact);
    let mut output = String::new();

    output.push_str("HayCut report\n");
    output.push_str("\nResultToken\n");
    output.push_str(&format!("  run: {}\n", report.manifest.id));
    output.push_str(&format!("  command: {}\n", report.manifest.command));
    output.push_str(&format!(
        "  result: {} (exit {})\n",
        result_label(report.manifest.exit_code),
        report.manifest.exit_code
    ));
    output.push_str(&format!("  duration: {}ms\n", report.manifest.duration_ms));
    output.push_str(&format!(
        "  likely failure: {}\n",
        likely_failure(report).unwrap_or("none detected")
    ));
    output.push_str(&format!(
        "  token: {} packet tokens\n",
        format_count(packet_tokens)
    ));

    output.push_str("\nspendReductionPreserved\n");
    output.push_str(&format!(
        "  raw tokens: {}\n",
        format_count(report.packet.raw_tokens)
    ));
    output.push_str(&format!(
        "  packet tokens: {}\n",
        format_count(packet_tokens)
    ));
    output.push_str(&format!(
        "  saved tokens: {}\n",
        format_count(raw_tokens_avoided)
    ));
    output.push_str(&format!(
        "  reduction: {:.1}%\n",
        reduction_percent(report.packet.raw_tokens, packet_tokens)
    ));
    output.push_str(&format!(
        "  budget: {} (soft {}, hard {})\n",
        budget_status_label(budget.status),
        format_count(budget.soft_budget),
        format_count(budget.hard_budget)
    ));
    output.push_str("  preserved:\n");
    for item in preserved_summary(report) {
        output.push_str(&format!("    - {item}\n"));
    }
    if !report.symbols.is_empty() {
        output.push_str("  symbols:\n");
        for symbol in &report.symbols {
            output.push_str(&format!(
                "    - {}::{} lines {}-{} ({} tokens)\n",
                symbol.path,
                symbol.symbol.name,
                symbol.symbol.start_line,
                symbol.symbol.end_line,
                format_count(symbol.estimated_tokens)
            ));
        }
    }

    output.push_str("\nevidenceOmitted\n");
    append_omitted_items(&mut output, &report.packet.omitted_items);

    output.push_str("\ncontextArtefacts\n");
    output.push_str(&format!("  stdout: {}\n", stdout_path.display()));
    output.push_str(&format!("  stderr: {}\n", stderr_path.display()));
    output.push_str(&format!("  compact: {}\n", compact_path.display()));

    output.push_str("\npreservedEvidence\n");
    append_preserved_items(&mut output, &report.packet.preserved_items);
    if !report.symbols.is_empty() {
        output.push_str("  symbol snippets:\n");
        for symbol in &report.symbols {
            output.push_str(&format!(
                "    {}::{} lines {}-{}\n",
                symbol.path, symbol.symbol.name, symbol.symbol.start_line, symbol.symbol.end_line
            ));
            output.push_str("    <code>\n");
            for line in symbol.code.lines() {
                output.push_str("    ");
                output.push_str(line);
                output.push('\n');
            }
            output.push_str("    </code>\n");
        }
    }

    output
}

fn append_preserved_items(output: &mut String, items: &[PreservedItem]) {
    if items.is_empty() {
        output.push_str("  none detected\n");
        return;
    }

    for item in items.iter().take(MAX_PRESERVED_ITEMS) {
        output.push_str(&format!(
            "  - {} {}: {}\n",
            source_label(item.source),
            preserved_kind_label(item.kind),
            item.line
        ));
    }
    append_remaining_count(output, items.len(), MAX_PRESERVED_ITEMS);
}

fn append_omitted_items(output: &mut String, items: &[OmittedItem]) {
    let omitted: Vec<&OmittedItem> = items.iter().filter(|item| item.count > 0).collect();
    if omitted.is_empty() {
        output.push_str("  none\n");
        return;
    }

    for item in omitted.iter().take(MAX_OMITTED_ITEMS) {
        output.push_str(&format!(
            "  - {}: {} lines ({})\n",
            source_label(item.source),
            format_count(item.count),
            item.reason
        ));
    }
    append_remaining_count(output, omitted.len(), MAX_OMITTED_ITEMS);
}

fn append_markdown_preserved_items(output: &mut String, items: &[PreservedItem]) {
    if items.is_empty() {
        output.push_str("- No compact evidence lines detected.\n");
        return;
    }

    for item in items.iter().take(MAX_PRESERVED_ITEMS) {
        output.push_str(&format!(
            "- `{}` `{}`: {}\n",
            source_label(item.source),
            preserved_kind_label(item.kind),
            markdown_inline_code(&item.line)
        ));
    }
    if items.len() > MAX_PRESERVED_ITEMS {
        output.push_str(&format!(
            "- ... {} more preserved evidence lines\n",
            format_count(items.len() - MAX_PRESERVED_ITEMS)
        ));
    }
}

fn append_markdown_omitted_items(output: &mut String, items: &[OmittedItem]) {
    let omitted: Vec<&OmittedItem> = items.iter().filter(|item| item.count > 0).collect();
    if omitted.is_empty() {
        output.push_str("- None reported.\n");
        return;
    }

    for item in omitted.iter().take(MAX_OMITTED_ITEMS) {
        output.push_str(&format!(
            "- `{}`: {} lines ({})\n",
            source_label(item.source),
            format_count(item.count),
            item.reason
        ));
    }
    if omitted.len() > MAX_OMITTED_ITEMS {
        output.push_str(&format!(
            "- ... {} more omission groups\n",
            format_count(omitted.len() - MAX_OMITTED_ITEMS)
        ));
    }
}

fn append_markdown_symbols(output: &mut String, report: &Report) {
    if report.symbols.is_empty() {
        return;
    }

    output.push_str("\n**Symbols**\n\n");
    for symbol in &report.symbols {
        output.push_str(&format!(
            "- `{}` lines {}-{} ({} tokens)\n",
            markdown_inline_code(&format!("{}::{}", symbol.path, symbol.symbol.name)),
            symbol.symbol.start_line,
            symbol.symbol.end_line,
            format_count(symbol.estimated_tokens)
        ));
    }
}

fn markdown_inline_code(value: &str) -> String {
    format!("`{}`", value.replace('`', "'"))
}

fn markdown_link_target(path: &Path) -> String {
    path.display().to_string().replace(' ', "%20")
}

fn append_remaining_count(output: &mut String, total: usize, limit: usize) {
    if total > limit {
        output.push_str(&format!("  - ... {} more\n", format_count(total - limit)));
    }
}

fn load_symbols(root: &Path, symbol_targets: &[String]) -> io::Result<Vec<SymbolMatch>> {
    symbol_targets
        .iter()
        .map(|target| read_symbol::read_symbol(root, target))
        .collect()
}

fn evidence_packet_tokens(report: &Report) -> usize {
    report.packet.packet_tokens
        + report
            .symbols
            .iter()
            .map(|symbol| symbol.estimated_tokens)
            .sum::<usize>()
}

fn likely_failure(report: &Report) -> Option<&str> {
    report
        .packet
        .preserved_items
        .iter()
        .find(|item| item.kind == PreservedKind::ErrorLine && !item.line.trim().is_empty())
        .map(|item| item.line.as_str())
}

fn reduction_percent(raw_tokens: usize, packet_tokens: usize) -> f64 {
    if raw_tokens == 0 {
        return 0.0;
    }

    let reduced_tokens = raw_tokens.saturating_sub(packet_tokens);
    reduced_tokens as f64 / raw_tokens as f64 * 100.0
}

fn preserved_summary(report: &Report) -> Vec<&'static str> {
    let mut items = vec![
        "exit code",
        "duration",
        "command metadata",
        "full output handle",
    ];

    if report
        .packet
        .preserved_items
        .iter()
        .any(|item| item.kind == PreservedKind::ErrorLine)
    {
        items.push("failing lines");
        items.push("stack trace frames");
    }

    items
}

fn result_label(exit_code: i32) -> &'static str {
    if exit_code == 0 { "passed" } else { "failed" }
}

fn budget_status_label(status: BudgetStatus) -> &'static str {
    match status {
        BudgetStatus::Within => "within budget",
        BudgetStatus::SoftExceeded => "over soft budget",
        BudgetStatus::HardExceeded => "over hard budget",
    }
}

fn source_label(source: OutputSource) -> &'static str {
    match source {
        OutputSource::Stdout => "stdout",
        OutputSource::Stderr => "stderr",
        OutputSource::Rtk => "rtk",
    }
}

fn preserved_kind_label(kind: PreservedKind) -> &'static str {
    match kind {
        PreservedKind::ErrorLine => "error",
        PreservedKind::StackTrace => "stack trace",
        PreservedKind::RtkFiltered => "filtered",
    }
}

fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);

    for (index, digit) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(digit);
    }

    formatted.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        time::{SystemTime, UNIX_EPOCH},
    };

    use chrono::Utc;

    use super::*;
    use crate::compactor::{OmittedItem, OutputSource, PreservedItem};
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
            "2026-07-07T15:29:00+00:00",
            &older,
        )
        .expect("older run should insert");
        insert_report_run(
            &db_path,
            "newer",
            "newer command",
            "2026-07-07T15:30:00+00:00",
            &latest,
        )
        .expect("newer run should insert");

        let report = load_last_report(&db_path, &[]).expect("last report should load from SQLite");

        assert_eq!(report.run_directory, latest);
        assert_eq!(report.manifest.id, "newer");
        assert_eq!(report.manifest.command, "newer command");
        assert_eq!(report.packet.packet_tokens, 10);

        fs::remove_dir_all(root).expect("test run root should be removed");
    }

    #[test]
    fn calculates_reduction_percentage() {
        assert_eq!(reduction_percent(0, 10), 0.0);
        assert_eq!(reduction_percent(100, 25), 75.0);
        assert_eq!(reduction_percent(100, 125), 0.0);
    }

    #[test]
    fn preserved_summary_includes_failure_details_when_error_lines_exist() {
        let report = Report {
            run_directory: PathBuf::from("run"),
            manifest: RunManifest {
                id: "2026-07-07T153000Z-a1b2c3".to_string(),
                command: "cargo test".to_string(),
                args: vec!["test".to_string()],
                cwd: "/tmp".to_string(),
                exit_code: 101,
                duration_ms: 42,
                stdout_bytes: 0,
                stderr_bytes: 0,
                estimated_raw_tokens: 100,
                raw_stdout_tokens_estimated: 80,
                raw_stderr_tokens_estimated: 20,
                created_at: Utc::now(),
                stdout: "stdout.txt".to_string(),
                stderr: "stderr.txt".to_string(),
                compact: "compact.json".to_string(),
            },
            packet: CompactPacket {
                compactor: "native".to_string(),
                rtk_version: None,
                command: "cargo test".to_string(),
                exit_code: 101,
                duration_ms: 42,
                failed: true,
                stdout_artifact: "stdout.txt".to_string(),
                stderr_artifact: "stderr.txt".to_string(),
                compact_artifact: None,
                raw_stdout_tokens: 80,
                raw_stderr_tokens: 20,
                raw_tokens: 100,
                packet_tokens: 10,
                preserved_items: vec![PreservedItem {
                    source: OutputSource::Stderr,
                    kind: PreservedKind::ErrorLine,
                    line: "error: failed".to_string(),
                }],
                omitted_items: vec![OmittedItem {
                    source: OutputSource::Stdout,
                    reason: "noise".to_string(),
                    count: 3,
                }],
                notes: Vec::new(),
                compact_text: None,
            },
            symbols: Vec::new(),
        };

        let summary = preserved_summary(&report);

        assert!(summary.contains(&"exit code"));
        assert!(summary.contains(&"duration"));
        assert!(summary.contains(&"failing lines"));
        assert!(summary.contains(&"stack trace frames"));
        assert!(summary.contains(&"full output handle"));
    }

    #[test]
    fn formats_human_report_with_command_budget_omissions_and_handles() {
        let report = Report {
            run_directory: PathBuf::from(".haycut/runs/run-1"),
            manifest: RunManifest {
                id: "run-1".to_string(),
                command: "cargo test auth".to_string(),
                args: vec!["test".to_string(), "auth".to_string()],
                cwd: "/tmp".to_string(),
                exit_code: 101,
                duration_ms: 42,
                stdout_bytes: 0,
                stderr_bytes: 0,
                estimated_raw_tokens: 1000,
                raw_stdout_tokens_estimated: 800,
                raw_stderr_tokens_estimated: 200,
                created_at: Utc::now(),
                stdout: "stdout.txt".to_string(),
                stderr: "stderr.txt".to_string(),
                compact: "compact.json".to_string(),
            },
            packet: CompactPacket {
                compactor: "native".to_string(),
                rtk_version: None,
                command: "cargo test auth".to_string(),
                exit_code: 101,
                duration_ms: 42,
                failed: true,
                stdout_artifact: "stdout.txt".to_string(),
                stderr_artifact: "stderr.txt".to_string(),
                compact_artifact: None,
                raw_stdout_tokens: 800,
                raw_stderr_tokens: 200,
                raw_tokens: 1000,
                packet_tokens: 60,
                preserved_items: vec![PreservedItem {
                    source: OutputSource::Stderr,
                    kind: PreservedKind::ErrorLine,
                    line: "tests/auth/session_test.rs:52 expected expired session to be rejected"
                        .to_string(),
                }],
                omitted_items: vec![OmittedItem {
                    source: OutputSource::Stdout,
                    reason: "non-error output omitted from compact packet".to_string(),
                    count: 37,
                }],
                notes: Vec::new(),
                compact_text: None,
            },
            symbols: Vec::new(),
        };

        let packet = human_report(&report, &token_config());

        assert!(packet.contains("HayCut report"));
        assert!(packet.contains("ResultToken"));
        assert!(packet.contains("command: cargo test auth"));
        assert!(packet.contains("result: failed (exit 101)"));
        assert!(packet.contains("likely failure: tests/auth/session_test.rs:52"));
        assert!(packet.contains("spendReductionPreserved"));
        assert!(packet.contains("raw tokens: 1,000"));
        assert!(packet.contains("packet tokens: 60"));
        assert!(packet.contains("saved tokens: 940"));
        assert!(packet.contains("budget: within budget"));
        assert!(packet.contains("evidenceOmitted"));
        assert!(packet.contains("stdout: 37 lines"));
        assert!(packet.contains("contextArtefacts"));
        assert!(packet.contains("stdout: .haycut/runs/run-1/stdout.txt"));
        assert!(packet.contains("stderr: .haycut/runs/run-1/stderr.txt"));
        assert!(packet.contains("compact: .haycut/runs/run-1/compact.json"));
        assert!(packet.contains("preservedEvidence"));
        assert!(packet.contains("stderr error: tests/auth/session_test.rs:52"));
    }

    #[test]
    fn formats_human_report_with_symbol_snippets() {
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

        let packet = human_report(&report, &token_config());

        assert!(packet.contains("src/auth/session.rs::validate_session lines 88-90"));
        assert!(packet.contains("fn validate_session() -> bool"));
        assert!(packet.contains("token: 72 packet tokens"));
    }

    #[test]
    fn formats_json_report_with_token_estimates_and_artifact_paths_without_raw_output() {
        let mut report = report_fixture();
        report.packet.omitted_items.push(OmittedItem {
            source: OutputSource::Stdout,
            reason: "non-error output omitted from compact packet".to_string(),
            count: 37,
        });
        let rendered = serde_json::to_string(&json_report(&report, &token_config()))
            .expect("JSON report should serialize");
        let value: serde_json::Value =
            serde_json::from_str(&rendered).expect("JSON report should parse");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["run"]["id"], "run-1");
        assert_eq!(value["run"]["command"], "cargo test auth");
        assert_eq!(value["token_estimates"]["raw_total"], 1000);
        assert_eq!(value["token_estimates"]["raw_stdout"], 800);
        assert_eq!(value["token_estimates"]["raw_stderr"], 200);
        assert_eq!(value["token_estimates"]["packet"], 60);
        assert_eq!(value["token_estimates"]["saved"], 940);
        assert_eq!(value["reduction_percent"], 94.0);
        assert_eq!(
            value["artefacts"]["stdout"],
            ".haycut/runs/run-1/stdout.txt"
        );
        assert_eq!(
            value["artefacts"]["stderr"],
            ".haycut/runs/run-1/stderr.txt"
        );
        assert_eq!(
            value["artefacts"]["compact"],
            ".haycut/runs/run-1/compact.json"
        );
        assert_eq!(value["omitted_evidence"][0]["source"], "stdout");
        assert_eq!(value["omitted_evidence"][0]["count"], 37);
        assert!(!rendered.contains("expected expired session to be rejected"));
        assert!(!rendered.contains("preserved_items"));
    }

    #[test]
    fn formats_markdown_report_with_result_savings_evidence_and_artifact_links() {
        let mut report = report_fixture();
        report.packet.omitted_items.push(OmittedItem {
            source: OutputSource::Stdout,
            reason: "non-error output omitted from compact packet".to_string(),
            count: 37,
        });

        let rendered = markdown_report(&report, &token_config());

        assert!(rendered.starts_with("# HayCut Report"));
        assert!(rendered.contains("## Result"));
        assert!(rendered.contains("- **Command:** `cargo test auth`"));
        assert!(rendered.contains("- **Result:** failed (exit `101`)"));
        assert!(rendered.contains("## Token Savings"));
        assert!(rendered.contains("| Raw tokens | 1,000 |"));
        assert!(rendered.contains("| Packet tokens | 60 |"));
        assert!(rendered.contains("| Saved tokens | 940 |"));
        assert!(rendered.contains("| Reduction | 94.0% |"));
        assert!(rendered.contains("## Evidence Summary"));
        assert!(rendered.contains("- failing lines"));
        assert!(rendered.contains("- `stderr` `error`: `tests/auth/session_test.rs:52"));
        assert!(rendered.contains("- `stdout`: 37 lines"));
        assert!(rendered.contains("## Full Artefacts"));
        assert!(rendered.contains("- [stdout](.haycut/runs/run-1/stdout.txt)"));
        assert!(rendered.contains("- [stderr](.haycut/runs/run-1/stderr.txt)"));
        assert!(rendered.contains("- [compact packet](.haycut/runs/run-1/compact.json)"));
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

    fn report_fixture() -> Report {
        Report {
            run_directory: PathBuf::from(".haycut/runs/run-1"),
            manifest: RunManifest {
                id: "run-1".to_string(),
                command: "cargo test auth".to_string(),
                args: vec!["test".to_string(), "auth".to_string()],
                cwd: "/tmp".to_string(),
                exit_code: 101,
                duration_ms: 42,
                stdout_bytes: 0,
                stderr_bytes: 0,
                estimated_raw_tokens: 1000,
                raw_stdout_tokens_estimated: 800,
                raw_stderr_tokens_estimated: 200,
                created_at: Utc::now(),
                stdout: "stdout.txt".to_string(),
                stderr: "stderr.txt".to_string(),
                compact: "compact.json".to_string(),
            },
            packet: CompactPacket {
                compactor: "native".to_string(),
                rtk_version: None,
                command: "cargo test auth".to_string(),
                exit_code: 101,
                duration_ms: 42,
                failed: true,
                stdout_artifact: "stdout.txt".to_string(),
                stderr_artifact: "stderr.txt".to_string(),
                compact_artifact: None,
                raw_stdout_tokens: 800,
                raw_stderr_tokens: 200,
                raw_tokens: 1000,
                packet_tokens: 60,
                preserved_items: vec![PreservedItem {
                    source: OutputSource::Stderr,
                    kind: PreservedKind::ErrorLine,
                    line: "tests/auth/session_test.rs:52 expected expired session to be rejected"
                        .to_string(),
                }],
                omitted_items: Vec::new(),
                notes: Vec::new(),
                compact_text: None,
            },
            symbols: Vec::new(),
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
        let manifest = RunManifest {
            id: id.to_string(),
            command: command.to_string(),
            args: Vec::new(),
            cwd: "/tmp".to_string(),
            exit_code: 0,
            duration_ms: 42,
            stdout_bytes: 0,
            stderr_bytes: 0,
            estimated_raw_tokens: 100,
            raw_stdout_tokens_estimated: 80,
            raw_stderr_tokens_estimated: 20,
            created_at: Utc::now(),
            stdout: "stdout.txt".to_string(),
            stderr: "stderr.txt".to_string(),
            compact: "compact.json".to_string(),
        };
        let packet = CompactPacket {
            compactor: "native".to_string(),
            rtk_version: None,
            command: command.to_string(),
            exit_code: 0,
            duration_ms: 42,
            failed: false,
            stdout_artifact: run_directory.join("stdout.txt").display().to_string(),
            stderr_artifact: run_directory.join("stderr.txt").display().to_string(),
            compact_artifact: None,
            raw_stdout_tokens: 80,
            raw_stderr_tokens: 20,
            raw_tokens: 100,
            packet_tokens,
            preserved_items: Vec::new(),
            omitted_items: Vec::new(),
            notes: Vec::new(),
            compact_text: None,
        };

        fs::write(
            run_directory.join("run.json"),
            serde_json::to_string_pretty(&manifest).map_err(io::Error::other)?,
        )?;
        fs::write(
            run_directory.join("compact.json"),
            serde_json::to_string_pretty(&packet).map_err(io::Error::other)?,
        )
    }

    fn insert_report_run(
        db_path: &Path,
        id: &str,
        command: &str,
        created_at: &str,
        run_directory: &Path,
    ) -> io::Result<()> {
        insert_run(
            db_path,
            &NewRun {
                id,
                command,
                cwd: "/tmp",
                exit_code: Some(0),
                duration_ms: 42,
                raw_tokens: 100,
                packet_tokens: 10,
                created_at,
                artifacts: vec![
                    NewArtifact {
                        id: format!("{id}:run_manifest"),
                        kind: "run_manifest",
                        path: run_directory.join("run.json").display().to_string(),
                        estimated_tokens: None,
                    },
                    NewArtifact {
                        id: format!("{id}:compact_json"),
                        kind: "compact_json",
                        path: run_directory.join("compact.json").display().to_string(),
                        estimated_tokens: Some(10),
                    },
                ],
            },
        )
    }
}
