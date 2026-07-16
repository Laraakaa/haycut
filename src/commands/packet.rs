use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    budget::BudgetUsage,
    code_context::{CodeContext, render_code_context},
    commands::{run_context::RunContext, trace::RunManifest},
    compactor::{CompactPacket, OutputSource},
    config::{Config, TokenConfig},
    extract::{DiagnosticKind, Severity},
    store::RUN_STORE_PATH,
    util::{estimate_tokens, format_count},
};

pub fn run(budget: Option<usize>, force: bool) -> i32 {
    let config = match Config::load_from_current_dir() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Error loading config: {error}");
            return 1;
        }
    };

    match load_last_failed_packet(Path::new(RUN_STORE_PATH)) {
        Ok(mut packet) => {
            if let Some(budget) = budget {
                packet.prune_to_budget(budget);
            }

            let budget = packet.budget_usage(&config.token);
            if let Some(error) = budget.hard_error().filter(|_| !force) {
                eprint!("{}", budget.render());
                eprintln!("Error: {error}");
                return 2;
            }

            print!("{}", packet.render(&config.token));
            0
        }
        Err(error) => {
            eprintln!("Error loading packet: {error}");
            1
        }
    }
}

#[derive(Debug)]
pub struct EvidencePacket {
    pub title: String,
    pub summary: Vec<String>,
    pub items: Vec<ContextItem>,
    pub omitted: Vec<OmittedItem>,
    pub raw_token_estimate: usize,
    pub base_token_estimate: usize,
    pub token_estimate: usize,
    pub full_handles: Vec<ArtifactHandle>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ContextItem {
    pub kind: ContextKind,
    pub content: String,
    pub source: SourceRef,
    pub reason: String,
    pub priority: Priority,
    pub token_estimate: usize,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextKind {
    CommandSummary,
    FailureLine,
    StackFrame,
    Assertion,
    FileReference,
    CodeWindow,
    Symbol,
    Diff,
    Note,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ArtifactHandle {
    pub kind: ArtifactKind,
    pub path: PathBuf,
    pub token_estimate: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactKind {
    Stdout,
    Stderr,
    Compact,
}

#[derive(Debug, PartialEq, Eq)]
pub struct OmittedItem {
    pub source: SourceRef,
    pub reason: String,
    pub count: usize,
    pub token_estimate: usize,
}

#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub enum SourceRef {
    Run {
        id: String,
    },
    Output {
        kind: ArtifactKind,
    },
    File {
        path: String,
        line: usize,
    },
    CodeWindow {
        path: String,
        start_line: usize,
        end_line: usize,
    },
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    High,
    Medium,
    Low,
}

impl EvidencePacket {
    fn render(&self, token_config: &TokenConfig) -> String {
        let mut output = String::new();

        output.push_str(&self.title);
        output.push('\n');
        for line in &self.summary {
            output.push_str(line);
            output.push('\n');
        }

        output.push_str("Diagnostics:\n");
        let diagnostics = self.items_of(ContextKind::FailureLine);
        if diagnostics.is_empty() {
            output.push_str("  none detected\n");
        } else {
            for item in diagnostics {
                output.push_str(&format!("  - {}\n", item.content));
            }
        }

        output.push_str("Files mentioned:\n");
        let file_references = self.items_of(ContextKind::FileReference);
        if file_references.is_empty() {
            output.push_str("  none detected\n");
        } else {
            for item in file_references {
                output.push_str(&format!("  {}\n", item.content));
            }
        }

        output.push_str("Suggested context:\n");
        let windows = self.items_of(ContextKind::CodeWindow);
        if windows.is_empty() {
            output.push_str("  none\n");
        } else {
            for item in windows {
                let SourceRef::CodeWindow {
                    path,
                    start_line,
                    end_line: _,
                } = &item.source
                else {
                    continue;
                };
                output.push_str(&render_code_context(CodeContext {
                    symbol: Some("window"),
                    path: Some(path),
                    start_line: Some(*start_line),
                    source: &item.content,
                    semantic_label: None,
                }));
                output.push_str(&format!("reason: {}\n", item.reason));
            }
        }

        output.push_str("Context budget:\n");
        output.push_str(&format!(
            "  packet tokens: {}\n",
            format_count(self.token_estimate)
        ));
        output.push_str(&self.budget_usage(token_config).render());
        if !self.omitted.is_empty() {
            output.push_str("Omitted:\n");
            for item in &self.omitted {
                output.push_str(&format!(
                    "  - source: {}  tokens: {}  reason: {}\n",
                    item.source.label(),
                    format_count(item.token_estimate),
                    item.reason
                ));
            }
        }
        output.push_str("Full handles:\n");
        for handle in &self.full_handles {
            output.push_str(&format!(
                "  {}: {}\n",
                handle.kind.label(),
                handle.path.display()
            ));
        }

        output
    }

    fn budget_usage(&self, token_config: &TokenConfig) -> BudgetUsage {
        BudgetUsage::from_config(token_config, self.raw_token_estimate, self.token_estimate)
    }

    fn prune_to_budget(&mut self, budget: usize) {
        let mut fixed_items = Vec::new();
        let mut prunable_items = Vec::new();
        for (index, item) in self.items.drain(..).enumerate() {
            if item.is_prunable_context() {
                prunable_items.push((index, item));
            } else {
                fixed_items.push((index, item));
            }
        }

        prunable_items.sort_by_key(|(index, item)| (item.priority.prune_rank(), *index));
        let mut token_estimate = self.base_token_estimate;
        let mut kept_items = fixed_items;

        for (index, item) in prunable_items {
            if token_estimate + item.token_estimate <= budget {
                token_estimate += item.token_estimate;
                kept_items.push((index, item));
            } else {
                self.omitted.push(OmittedItem {
                    source: item.source,
                    reason: format!("over budget; {}", item.reason),
                    count: 0,
                    token_estimate: item.token_estimate,
                });
            }
        }

        kept_items.sort_by_key(|(index, item)| (item.priority.render_rank(), *index));
        self.items = kept_items.into_iter().map(|(_, item)| item).collect();
        self.token_estimate = token_estimate;
    }

    fn items_of(&self, kind: ContextKind) -> Vec<&ContextItem> {
        self.items.iter().filter(|item| item.kind == kind).collect()
    }
}

impl ContextItem {
    fn is_prunable_context(&self) -> bool {
        matches!(
            self.kind,
            ContextKind::FileReference | ContextKind::CodeWindow
        )
    }
}

impl Priority {
    fn prune_rank(self) -> u8 {
        match self {
            Priority::High => 0,
            Priority::Medium => 1,
            Priority::Low => 2,
        }
    }

    fn render_rank(self) -> u8 {
        self.prune_rank()
    }
}

impl ArtifactKind {
    fn label(self) -> &'static str {
        match self {
            ArtifactKind::Stdout => "stdout",
            ArtifactKind::Stderr => "stderr",
            ArtifactKind::Compact => "compact",
        }
    }
}

impl SourceRef {
    fn label(&self) -> String {
        match self {
            SourceRef::Run { id } => format!("run {id}"),
            SourceRef::Output { kind } => kind.label().to_string(),
            SourceRef::File { path, line } => format!("{path}:{line}"),
            SourceRef::CodeWindow {
                path,
                start_line,
                end_line,
            } => format!("{path} lines {start_line}-{end_line}"),
        }
    }
}

fn load_last_failed_packet(db_path: &Path) -> io::Result<EvidencePacket> {
    let ctx = RunContext::load_last_failed(db_path)?;
    Ok(build_evidence_packet(
        &ctx.manifest,
        &ctx.evidence,
        &ctx.compact,
    ))
}

/// Render the shared evidence layer's likely failure as a single line,
/// matching `haycut report` so both commands agree on the same run.
fn format_likely_failure(evidence: &crate::evidence::EvidencePacket) -> String {
    match &evidence.likely_failure {
        Some(failure) => format!("{}: {}", failure.kind, failure.summary),
        None => "none detected".to_string(),
    }
}

/// Assemble a budget-aware packet purely from `evidence.json`: structured
/// diagnostics, referenced files, and the suggested context windows (whose
/// source lines are read from disk). This keeps `packet` a renderer of the
/// shared evidence layer, consistent with `report`.
fn build_evidence_packet(
    manifest: &RunManifest,
    evidence: &crate::evidence::EvidencePacket,
    compact: &CompactPacket,
) -> EvidencePacket {
    let root = PathBuf::from(&manifest.cwd);
    let mut items: Vec<ContextItem> = evidence.diagnostics.iter().map(diagnostic_item).collect();
    items.extend(evidence.file_refs.iter().map(file_ref_item));
    for context in &evidence.context_items {
        if let Some(item) = context_window_item(&root, context) {
            items.push(item);
        }
    }

    let summary = vec![
        format!("Run:      {}", manifest.id),
        format!(
            "Command:  {}  exit code: {}",
            manifest.command, manifest.exit_code
        ),
        format!("Likely failure:  {}", format_likely_failure(evidence)),
    ];
    let mention_tokens = items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                ContextKind::FileReference | ContextKind::CodeWindow
            )
        })
        .map(|item| item.token_estimate)
        .sum::<usize>();
    let base_token_estimate = evidence.token_summary.packet_tokens;
    let token_estimate = base_token_estimate + mention_tokens;
    let full_handles = artifact_handles(manifest, compact);
    let omitted = compact
        .omitted_items
        .iter()
        .map(|item| OmittedItem {
            source: source_ref_for_output(item.source),
            reason: item.reason.clone(),
            count: item.count,
            token_estimate: omitted_token_estimate(compact, item.source),
        })
        .collect();

    EvidencePacket {
        title: "EVIDENCE PACKET".to_string(),
        summary,
        items,
        omitted,
        raw_token_estimate: evidence.token_summary.raw_tokens,
        base_token_estimate,
        token_estimate,
        full_handles,
    }
}

fn diagnostic_item(diagnostic: &crate::evidence::EvidenceDiagnostic) -> ContextItem {
    let content = format_diagnostic(diagnostic);
    let source = match (&diagnostic.file, diagnostic.line) {
        (Some(path), Some(line)) => SourceRef::File {
            path: path.clone(),
            line,
        },
        _ => SourceRef::Output {
            kind: ArtifactKind::Compact,
        },
    };

    ContextItem {
        kind: ContextKind::FailureLine,
        token_estimate: estimate_tokens(content.as_bytes()),
        content,
        source,
        reason: "diagnostic extracted from run output".to_string(),
        priority: Priority::High,
    }
}

fn file_ref_item(reference: &crate::evidence::FileRef) -> ContextItem {
    let content = match reference.column {
        Some(column) => format!("{}:{}:{}", reference.file, reference.line, column),
        None => format!("{}:{}", reference.file, reference.line),
    };

    ContextItem {
        kind: ContextKind::FileReference,
        token_estimate: estimate_tokens(content.as_bytes()),
        content,
        source: SourceRef::File {
            path: reference.file.clone(),
            line: reference.line,
        },
        reason: reference.reason.clone(),
        priority: Priority::Medium,
    }
}

fn context_window_item(root: &Path, context: &crate::evidence::ContextItem) -> Option<ContextItem> {
    let (path, start_line, end_line) = parse_window_target(&context.target)?;
    let lines = read_line_range(root, &path, start_line, end_line)?;
    let content = lines.join("\n");

    Some(ContextItem {
        kind: ContextKind::CodeWindow,
        token_estimate: estimate_tokens(content.as_bytes()),
        content,
        source: SourceRef::CodeWindow {
            path,
            start_line,
            end_line,
        },
        reason: context.reason.clone(),
        priority: Priority::Medium,
    })
}

/// Parse an evidence context target like `src/config.rs:203-223`.
fn parse_window_target(target: &str) -> Option<(String, usize, usize)> {
    let (path, range) = target.rsplit_once(':')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse::<usize>().ok()?;
    let end = end.parse::<usize>().ok()?;
    if start == 0 || end < start {
        return None;
    }

    Some((path.to_string(), start, end))
}

/// Read the inclusive `start..=end` line range from a source file, clamped to
/// the file's bounds. Returns `None` if the file is missing or the range is
/// out of bounds.
fn read_line_range(root: &Path, path: &str, start: usize, end: usize) -> Option<Vec<String>> {
    let candidate = Path::new(path);
    let source_path = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let source = fs::read_to_string(source_path).ok()?;
    let lines: Vec<&str> = source.lines().collect();
    if start == 0 || start > lines.len() {
        return None;
    }

    let end = end.min(lines.len());
    Some(
        lines[start - 1..end]
            .iter()
            .map(|line| (*line).to_string())
            .collect(),
    )
}

/// Render a structured diagnostic as a single readable line, e.g.
/// `error[E0063]: missing field ... (src/config.rs:54:9)`.
fn format_diagnostic(diagnostic: &crate::evidence::EvidenceDiagnostic) -> String {
    let head = match diagnostic.kind {
        DiagnosticKind::RustCompileError => match &diagnostic.code {
            Some(code) => format!("error[{code}]"),
            None => "error".to_string(),
        },
        DiagnosticKind::TestFailure => "test failure".to_string(),
        DiagnosticKind::Panic => "panic".to_string(),
        DiagnosticKind::Generic => match diagnostic.severity {
            Severity::Error => "error".to_string(),
            Severity::Warning => "warning".to_string(),
        },
    };

    let mut rendered = format!("{head}: {}", diagnostic.message);
    if let Some(location) = format_location(
        diagnostic.file.as_deref(),
        diagnostic.line,
        diagnostic.column,
    ) {
        rendered.push_str(&format!(" ({location})"));
    }
    rendered
}

fn format_location(
    file: Option<&str>,
    line: Option<usize>,
    column: Option<usize>,
) -> Option<String> {
    let file = file?;
    match (line, column) {
        (Some(line), Some(column)) => Some(format!("{file}:{line}:{column}")),
        (Some(line), None) => Some(format!("{file}:{line}")),
        _ => Some(file.to_string()),
    }
}

fn omitted_token_estimate(compact: &CompactPacket, source: OutputSource) -> usize {
    match source {
        OutputSource::Stdout => compact.raw_stdout_tokens,
        OutputSource::Stderr => compact.raw_stderr_tokens,
        OutputSource::Rtk => compact.packet_tokens,
    }
}

fn artifact_handles(manifest: &RunManifest, compact: &CompactPacket) -> Vec<ArtifactHandle> {
    vec![
        ArtifactHandle {
            kind: ArtifactKind::Stdout,
            path: PathBuf::from(&manifest.stdout),
            token_estimate: compact.raw_stdout_tokens,
        },
        ArtifactHandle {
            kind: ArtifactKind::Stderr,
            path: PathBuf::from(&manifest.stderr),
            token_estimate: compact.raw_stderr_tokens,
        },
        ArtifactHandle {
            kind: ArtifactKind::Compact,
            path: compact
                .compact_artifact
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&manifest.compact)),
            token_estimate: compact.packet_tokens,
        },
    ]
}

fn source_ref_for_output(source: OutputSource) -> SourceRef {
    let kind = match source {
        OutputSource::Stdout => ArtifactKind::Stdout,
        OutputSource::Stderr => ArtifactKind::Stderr,
        OutputSource::Rtk => ArtifactKind::Compact,
    };

    SourceRef::Output { kind }
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use chrono::Utc;

    use super::*;
    use crate::compactor::{NativeHeuristicCompactor, OutputCompactor};
    use crate::extract::{DiagnosticKind, Severity};
    use crate::store::{NewArtifact, NewRun, insert_run};

    #[test]
    fn loads_most_recent_failed_run_not_latest_success() {
        let root = temp_root("packet-last-failed");
        let db_path = root.join("haycut.sqlite3");
        let failed_run = root.join("failed-run");
        let success_run = root.join("success-run");
        write_run_artifacts(&failed_run, "failed", "cargo test auth", 101)
            .expect("failed run artifacts should be written");
        write_run_artifacts(&success_run, "success", "cargo test", 0)
            .expect("success run artifacts should be written");
        insert_packet_run(
            &db_path,
            "failed",
            "cargo test auth",
            101,
            "2026-07-07T15:29:00+00:00",
            &failed_run,
        )
        .expect("failed run should insert");
        insert_packet_run(
            &db_path,
            "success",
            "cargo test",
            0,
            "2026-07-07T15:30:00+00:00",
            &success_run,
        )
        .expect("success run should insert");

        let packet = load_last_failed_packet(&db_path).expect("last failed packet should load");

        assert!(packet.summary.iter().any(|line| line == "Run:      failed"));
        assert!(
            packet
                .summary
                .iter()
                .any(|line| line == "Command:  cargo test auth  exit code: 101")
        );

        fs::remove_dir_all(root).expect("test root should be removed");
    }

    #[test]
    fn renders_packet_from_evidence_diagnostics_files_and_context() {
        let root = temp_root("packet-render");
        fs::create_dir_all(root.join("tests/auth")).expect("source directory should be created");
        fs::write(
            root.join("tests/auth/session_test.rs"),
            "fn setup() {}\nfn helper() {}\nsetup();\nlet session = expired_session();\nassert!(validate_session(session).is_err());\nfn teardown() {}\n",
        )
        .expect("source should be written");

        let evidence = crate::evidence::EvidencePacket {
            schema_version: 1,
            run_id: "run-1".to_string(),
            outcome: crate::evidence::Outcome {
                exit_code: 101,
                status: "failure".to_string(),
            },
            likely_failure: Some(crate::evidence::LikelyFailure {
                kind: "test_failure".to_string(),
                summary: "Test auth session failed".to_string(),
                confidence: "high".to_string(),
            }),
            primary_diagnostic: Some(crate::evidence::PrimaryDiagnostic {
                source: "stderr".to_string(),
                message: "assertion failed: session rejected".to_string(),
                code: None,
                file: Some("tests/auth/session_test.rs".to_string()),
                line: Some(5),
                column: Some(5),
            }),
            diagnostics: vec![crate::evidence::EvidenceDiagnostic {
                kind: DiagnosticKind::Panic,
                severity: Severity::Error,
                code: None,
                message: "assertion failed: session rejected".to_string(),
                file: Some("tests/auth/session_test.rs".to_string()),
                line: Some(5),
                column: Some(5),
            }],
            file_refs: vec![crate::evidence::FileRef {
                file: "tests/auth/session_test.rs".to_string(),
                line: 5,
                column: Some(5),
                reason: "panic location".to_string(),
            }],
            context_items: vec![crate::evidence::ContextItem {
                kind: "file_window".to_string(),
                target: "tests/auth/session_test.rs:3-5".to_string(),
                reason: "Primary test failure location".to_string(),
            }],
            token_summary: crate::evidence::TokenSummary {
                raw_tokens: 100,
                packet_tokens: 25,
                saved_tokens: 75,
                reduction_percent: 75.0,
            },
        };
        let compact = compact_fixture("cargo test auth", 101, 25);
        let manifest =
            manifest_fixture("run-1", "cargo test auth", 101, &root.display().to_string());

        let packet = build_evidence_packet(&manifest, &evidence, &compact);
        let rendered = packet.render(&token_config());

        fs::remove_dir_all(root).expect("test root should be removed");

        assert!(rendered.contains("Command:  cargo test auth  exit code: 101"));
        assert!(rendered.contains("Likely failure:  test_failure: Test auth session failed"));
        assert!(rendered.contains("Diagnostics:"));
        assert!(rendered.contains(
            "  - panic: assertion failed: session rejected (tests/auth/session_test.rs:5:5)"
        ));
        assert!(rendered.contains("Files mentioned:"));
        assert!(rendered.contains("  tests/auth/session_test.rs:5:5"));
        assert!(rendered.contains("Suggested context:"));
        assert!(rendered.contains("window@tests/auth/session_test.rs:3\n```rust\n"));
        assert!(rendered.contains("reason: Primary test failure location"));
        assert!(rendered.contains("assert!(validate_session(session).is_err());"));
        assert!(rendered.contains("packet tokens:"));
        assert!(rendered.contains("Budget:  soft: 40,000  hard: 80,000"));
        assert!(rendered.contains("Status: packet is within budget"));
    }

    #[test]
    fn detects_hard_budget_exceeded_for_packet() {
        let evidence = crate::evidence::EvidencePacket {
            schema_version: 1,
            run_id: "run-1".to_string(),
            outcome: crate::evidence::Outcome {
                exit_code: 101,
                status: "failure".to_string(),
            },
            likely_failure: None,
            primary_diagnostic: None,
            diagnostics: Vec::new(),
            file_refs: Vec::new(),
            context_items: Vec::new(),
            token_summary: crate::evidence::TokenSummary {
                raw_tokens: 100,
                packet_tokens: 90_000,
                saved_tokens: 0,
                reduction_percent: 0.0,
            },
        };
        let compact = compact_fixture("cargo test auth", 101, 90_000);
        let manifest = manifest_fixture("run-1", "cargo test auth", 101, "/tmp");

        let packet = build_evidence_packet(&manifest, &evidence, &compact);

        assert!(packet.budget_usage(&token_config()).hard_error().is_some());
    }

    #[test]
    fn prunes_lower_priority_context_to_fit_budget() {
        let mut packet = EvidencePacket {
            title: "EVIDENCE PACKET".to_string(),
            summary: vec!["Run:      run-1".to_string()],
            items: vec![
                ContextItem {
                    kind: ContextKind::CommandSummary,
                    content: "cargo test exited with 101".to_string(),
                    source: SourceRef::Run {
                        id: "run-1".to_string(),
                    },
                    reason: "summarizes the captured command run".to_string(),
                    priority: Priority::High,
                    token_estimate: 6,
                },
                window_item_fixture("src/high.rs", 1, 20, Priority::High),
                window_item_fixture("src/medium.rs", 1, 15, Priority::Medium),
                window_item_fixture("src/low.rs", 1, 10, Priority::Low),
            ],
            omitted: Vec::new(),
            raw_token_estimate: 200,
            base_token_estimate: 10,
            token_estimate: 55,
            full_handles: Vec::new(),
        };

        packet.prune_to_budget(45);
        let rendered = packet.render(&token_config());

        assert_eq!(packet.token_estimate, 45);
        assert!(rendered.contains("window@src/high.rs:1\n```rust\n"));
        assert!(rendered.contains("window@src/medium.rs:1\n```rust\n"));
        assert!(!rendered.contains("window@src/low.rs:1\n```rust\n"));
        assert!(rendered.contains("Omitted:"));
        assert!(rendered.contains(
            "source: src/low.rs lines 1-1  tokens: 10  reason: over budget; nearby source context"
        ));
        assert!(rendered.contains("packet tokens: 45"));
    }

    #[test]
    fn renders_golden_packet_for_rust_failure_fixture() {
        let root = temp_root("golden-rust-packet");
        fs::create_dir_all(root.join("src/commands")).expect("source directory should be created");
        fs::write(
            root.join("src/commands/packet.rs"),
            source_with_line(
                610,
                &[
                    "#[test]",
                    "fn renders_packet() {",
                    "    assert_eq!(actual.source(), \"useful source excerpt\");",
                    "}",
                    "",
                ],
            ),
        )
        .expect("source fixture should be written");

        let stderr = include_bytes!("../../fixtures/outputs/rust_test_failure.stderr");
        let args = vec![
            "test".to_string(),
            "commands::packet::tests::renders_packet".to_string(),
        ];
        let stdout_artifact = PathBuf::from(".haycut/runs/golden-rust/stdout.txt");
        let stderr_artifact = PathBuf::from(".haycut/runs/golden-rust/stderr.txt");
        let input = crate::compactor::CompactionInput {
            command: "cargo",
            args: &args,
            exit_code: 101,
            duration: Duration::from_millis(42),
            stdout: b"",
            stderr,
            stdout_artifact: &stdout_artifact,
            stderr_artifact: &stderr_artifact,
        };
        let compact = NativeHeuristicCompactor
            .compact(&input)
            .expect("fixture should compact");
        let stderr_text = String::from_utf8_lossy(stderr).into_owned();
        let evidence = crate::evidence::build("golden-rust", 101, &compact, "", &stderr_text);
        let mut manifest = manifest_fixture(
            "golden-rust",
            "cargo test commands::packet::tests::renders_packet",
            101,
            &root.display().to_string(),
        );
        manifest.stdout = ".haycut/runs/golden-rust/stdout.txt".to_string();
        manifest.stderr = ".haycut/runs/golden-rust/stderr.txt".to_string();
        manifest.compact = "sqlite:compact_json".to_string();
        let packet = build_evidence_packet(&manifest, &evidence, &compact);
        let rendered = packet.render(&token_config());

        fs::remove_dir_all(root).expect("test root should be removed");
        insta::assert_snapshot!("rust_failure_packet", rendered);
    }

    fn window_item_fixture(
        path: &str,
        start_line: usize,
        token_estimate: usize,
        priority: Priority,
    ) -> ContextItem {
        ContextItem {
            kind: ContextKind::CodeWindow,
            content: path.to_string(),
            source: SourceRef::CodeWindow {
                path: path.to_string(),
                start_line,
                end_line: start_line,
            },
            reason: "nearby source context".to_string(),
            priority,
            token_estimate,
        }
    }

    fn compact_fixture(command: &str, exit_code: i32, packet_tokens: usize) -> CompactPacket {
        CompactPacket {
            compactor: "native".to_string(),
            rtk_version: None,
            command: command.to_string(),
            exit_code,
            duration_ms: 42,
            failed: exit_code != 0,
            stdout_artifact: "stdout.txt".to_string(),
            stderr_artifact: "stderr.txt".to_string(),
            compact_artifact: None,
            raw_stdout_tokens: 0,
            raw_stderr_tokens: 0,
            raw_tokens: 100,
            packet_tokens,
            preserved_items: Vec::new(),
            omitted_items: Vec::new(),
            notes: Vec::new(),
            compact_text: None,
        }
    }

    fn token_config() -> TokenConfig {
        TokenConfig {
            soft_budget: 40_000,
            hard_budget: 80_000,
        }
    }

    fn temp_root(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "haycut-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos()
        ))
    }

    fn source_with_line(target_line: usize, lines: &[&str]) -> String {
        let mut source = String::new();
        for line in 1..target_line {
            source.push_str(&format!("// filler line {line}\n"));
        }
        for line in lines {
            source.push_str(line);
            source.push('\n');
        }
        source
    }

    fn write_run_artifacts(
        run_directory: &Path,
        id: &str,
        command: &str,
        exit_code: i32,
    ) -> io::Result<()> {
        fs::create_dir_all(run_directory)?;
        fs::write(run_directory.join("stdout.txt"), "")?;
        fs::write(
            run_directory.join("stderr.txt"),
            "error at tests/auth/session_test.rs:52:9\n",
        )?;
        let _manifest = manifest_fixture(id, command, exit_code, "/tmp");
        let compact = CompactPacket {
            compactor: "native".to_string(),
            rtk_version: None,
            command: command.to_string(),
            exit_code,
            duration_ms: 42,
            failed: exit_code != 0,
            stdout_artifact: "stdout.txt".to_string(),
            stderr_artifact: "stderr.txt".to_string(),
            compact_artifact: None,
            raw_stdout_tokens: 0,
            raw_stderr_tokens: 10,
            raw_tokens: 10,
            packet_tokens: 5,
            preserved_items: Vec::new(),
            omitted_items: Vec::new(),
            notes: Vec::new(),
            compact_text: None,
        };

        let evidence = crate::evidence::build(
            id,
            exit_code,
            &compact,
            "",
            "error at tests/auth/session_test.rs:52:9\n",
        );
        let _ = evidence;
        Ok(())
    }

    fn insert_packet_run(
        db_path: &Path,
        id: &str,
        command: &str,
        exit_code: i32,
        created_at: &str,
        run_directory: &Path,
    ) -> io::Result<()> {
        let compact = CompactPacket {
            compactor: "native".to_string(),
            rtk_version: None,
            command: command.to_string(),
            exit_code,
            duration_ms: 42,
            failed: exit_code != 0,
            stdout_artifact: run_directory.join("stdout.txt").display().to_string(),
            stderr_artifact: run_directory.join("stderr.txt").display().to_string(),
            compact_artifact: None,
            raw_stdout_tokens: 0,
            raw_stderr_tokens: 10,
            raw_tokens: 10,
            packet_tokens: 5,
            preserved_items: Vec::new(),
            omitted_items: Vec::new(),
            notes: Vec::new(),
            compact_text: None,
        };
        let evidence = crate::evidence::build(
            id,
            exit_code,
            &compact,
            "",
            "error at tests/auth/session_test.rs:52:9\n",
        );
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
                exit_code: Some(exit_code),
                duration_ms: 42,
                stdout_bytes: 0,
                stderr_bytes: 45,
                raw_tokens: 10,
                raw_stdout_tokens: 0,
                raw_stderr_tokens: 10,
                packet_tokens: 5,
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
                        estimated_tokens: Some(0),
                    },
                    NewArtifact {
                        id: format!("{id}:stderr"),
                        kind: "stderr",
                        path: stderr_path.clone(),
                        estimated_tokens: Some(10),
                    },
                ],
            },
        )
    }

    fn manifest_fixture(id: &str, command: &str, exit_code: i32, cwd: &str) -> RunManifest {
        RunManifest {
            id: id.to_string(),
            command: command.to_string(),
            args: Vec::new(),
            cwd: cwd.to_string(),
            exit_code,
            duration_ms: 42,
            stdout_bytes: 0,
            stderr_bytes: 0,
            estimated_raw_tokens: 10,
            raw_stdout_tokens_estimated: 0,
            raw_stderr_tokens_estimated: 10,
            created_at: Utc::now(),
            stdout: "stdout.txt".to_string(),
            stderr: "stderr.txt".to_string(),
            compact: "compact.json".to_string(),
            evidence: "evidence.json".to_string(),
        }
    }
}
