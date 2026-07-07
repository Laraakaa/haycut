use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    commands::read_symbol::{self, SymbolMatch},
    commands::trace::RunManifest,
    compactor::{CompactPacket, PreservedKind},
    store::{self, RUN_STORE_PATH},
};

pub fn run(last: bool, symbols: Vec<String>) -> i32 {
    if !last {
        eprintln!("Error: report currently requires --last");
        return 2;
    }

    match load_last_report(Path::new(RUN_STORE_PATH), &symbols) {
        Ok(report) => {
            print_report(&report);
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

fn print_report(report: &Report) {
    print!("{}", evidence_packet(report));
}

fn evidence_packet(report: &Report) -> String {
    let packet_tokens = evidence_packet_tokens(report);
    let raw_tokens_avoided = report.packet.raw_tokens.saturating_sub(packet_tokens);
    let stdout_path = report.run_directory.join(&report.manifest.stdout);
    let stderr_path = report.run_directory.join(&report.manifest.stderr);
    let compact_path = report.run_directory.join(&report.manifest.compact);
    let mut output = String::new();

    output.push_str("EVIDENCE PACKET\n");
    output.push_str(&format!("Run:      {}\n", report.manifest.id));
    output.push_str(&format!(
        "Command:  {}  exit code: {}\n",
        report.manifest.command, report.manifest.exit_code
    ));
    output.push_str(&format!(
        "Likely failure:  {}\n",
        likely_failure(report).unwrap_or("none detected")
    ));
    output.push_str("Relevant symbols:\n");
    if report.symbols.is_empty() {
        output.push_str("  none requested\n");
    } else {
        for symbol in &report.symbols {
            output.push_str(&format!(
                "  {}::{} lines {}-{}  estimated tokens: {}\n",
                symbol.path,
                symbol.symbol.name,
                symbol.symbol.start_line,
                symbol.symbol.end_line,
                format_count(symbol.estimated_tokens)
            ));
            output.push_str("  <code>\n");
            for line in symbol.code.lines() {
                output.push_str("  ");
                output.push_str(line);
                output.push('\n');
            }
            output.push_str("  </code>\n");
        }
    }
    output.push_str("Context budget:\n");
    output.push_str(&format!(
        "  packet tokens: {}\n",
        format_count(packet_tokens)
    ));
    output.push_str(&format!(
        "  raw source/log tokens avoided: {}\n",
        format_count(raw_tokens_avoided)
    ));
    output.push_str(&format!(
        "  reduction: {:.1}%\n",
        reduction_percent(report.packet.raw_tokens, packet_tokens)
    ));
    output.push_str("Full handles:\n");
    output.push_str(&format!("  stdout: {}\n", stdout_path.display()));
    output.push_str(&format!("  stderr: {}\n", stderr_path.display()));
    output.push_str(&format!("  compact: {}\n", compact_path.display()));
    output.push_str("Preserved:\n");
    for item in preserved_summary(report) {
        output.push_str(&format!("  {item}\n"));
    }

    output
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
    fn formats_evidence_packet_with_command_budget_and_handles() {
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
                omitted_items: Vec::new(),
                notes: Vec::new(),
                compact_text: None,
            },
            symbols: Vec::new(),
        };

        let packet = evidence_packet(&report);

        assert!(packet.contains("EVIDENCE PACKET"));
        assert!(packet.contains("Command:  cargo test auth  exit code: 101"));
        assert!(packet.contains("Likely failure:  tests/auth/session_test.rs:52"));
        assert!(packet.contains("packet tokens: 60"));
        assert!(packet.contains("raw source/log tokens avoided: 940"));
        assert!(packet.contains("stdout: .haycut/runs/run-1/stdout.txt"));
        assert!(packet.contains("stderr: .haycut/runs/run-1/stderr.txt"));
        assert!(packet.contains("compact: .haycut/runs/run-1/compact.json"));
    }

    #[test]
    fn formats_evidence_packet_with_symbol_snippets() {
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

        let packet = evidence_packet(&report);

        assert!(packet.contains("src/auth/session.rs::validate_session lines 88-90"));
        assert!(packet.contains("fn validate_session() -> bool"));
        assert!(packet.contains("packet tokens: 72"));
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
