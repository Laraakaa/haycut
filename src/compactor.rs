use std::{
    io::{self, Write},
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::util::estimate_tokens;

#[derive(Debug)]
pub struct CompactionInput<'a> {
    pub command: &'a str,
    pub args: &'a [String],
    pub exit_code: i32,
    pub duration: Duration,
    pub stdout: &'a [u8],
    pub stderr: &'a [u8],
    pub stdout_artifact: &'a PathBuf,
    pub stderr_artifact: &'a PathBuf,
}

pub trait OutputCompactor {
    fn name(&self) -> &'static str;
    fn compact(&self, input: &CompactionInput<'_>) -> io::Result<CompactPacket>;
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CompactPacket {
    pub compactor: String,
    pub rtk_version: Option<String>,
    pub command: String,
    pub exit_code: i32,
    pub duration_ms: u128,
    pub failed: bool,
    pub stdout_artifact: String,
    pub stderr_artifact: String,
    pub compact_artifact: Option<String>,
    pub raw_stdout_tokens: usize,
    pub raw_stderr_tokens: usize,
    pub raw_tokens: usize,
    pub packet_tokens: usize,
    pub preserved_items: Vec<PreservedItem>,
    pub omitted_items: Vec<OmittedItem>,
    pub notes: Vec<String>,
    #[serde(skip)]
    pub compact_text: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PreservedItem {
    pub source: OutputSource,
    pub kind: PreservedKind,
    pub line: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OmittedItem {
    pub source: OutputSource,
    pub reason: String,
    pub count: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputSource {
    Stdout,
    Stderr,
    Rtk,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreservedKind {
    ErrorLine,
    StackTrace,
    RtkFiltered,
}

pub struct NativeHeuristicCompactor;

impl OutputCompactor for NativeHeuristicCompactor {
    fn name(&self) -> &'static str {
        "native"
    }

    fn compact(&self, input: &CompactionInput<'_>) -> io::Result<CompactPacket> {
        let mut preserved_items = Vec::new();
        let stdout_lines =
            collect_native_items(input.stdout, OutputSource::Stdout, &mut preserved_items);
        let stderr_lines =
            collect_native_items(input.stderr, OutputSource::Stderr, &mut preserved_items);
        let omitted_items = omitted_items(stdout_lines, stderr_lines, &preserved_items);
        let raw_stdout_tokens = estimate_tokens(input.stdout);
        let raw_stderr_tokens = estimate_tokens(input.stderr);
        let mut notes = vec![format!(
            "exit code: {}, duration: {}ms",
            input.exit_code,
            input.duration.as_millis()
        )];

        if input.exit_code != 0 {
            notes.push("command failed".to_string());
        }

        let packet_tokens = estimate_packet_tokens(&preserved_items, &omitted_items, &notes);

        Ok(CompactPacket {
            compactor: self.name().to_string(),
            rtk_version: None,
            command: command_line(input),
            exit_code: input.exit_code,
            duration_ms: input.duration.as_millis(),
            failed: input.exit_code != 0,
            stdout_artifact: input.stdout_artifact.display().to_string(),
            stderr_artifact: input.stderr_artifact.display().to_string(),
            compact_artifact: None,
            raw_stdout_tokens,
            raw_stderr_tokens,
            raw_tokens: raw_stdout_tokens + raw_stderr_tokens,
            packet_tokens,
            preserved_items,
            omitted_items,
            notes,
            compact_text: None,
        })
    }
}

pub struct RtkCompactor;

impl RtkCompactor {
    pub fn is_installed() -> bool {
        Self::version().is_ok()
    }

    pub fn version() -> io::Result<String> {
        let output = Command::new("rtk").arg("--version").output()?;

        if !output.status.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

impl OutputCompactor for RtkCompactor {
    fn name(&self) -> &'static str {
        "rtk-pipe"
    }

    fn compact(&self, input: &CompactionInput<'_>) -> io::Result<CompactPacket> {
        let version = Self::version()?;
        let filter = rtk_filter_name(input).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!("no RTK pipe filter for {}", command_line(input)),
            )
        })?;

        let mut child = Command::new("rtk")
            .args(["pipe", "--filter", filter])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.stdout)?;
            stdin.write_all(input.stderr)?;
        }

        let output = child.wait_with_output()?;

        if !output.status.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }

        let compact_output = [output.stdout, output.stderr].concat();
        let mut preserved_items = Vec::new();
        for line in String::from_utf8_lossy(&compact_output).lines().take(120) {
            preserved_items.push(PreservedItem {
                source: OutputSource::Rtk,
                kind: PreservedKind::RtkFiltered,
                line: line.to_string(),
            });
        }

        let raw_stdout_tokens = estimate_tokens(input.stdout);
        let raw_stderr_tokens = estimate_tokens(input.stderr);
        let compact_text = String::from_utf8_lossy(&compact_output).into_owned();
        let notes = vec![format!("rtk version: {version}")];

        Ok(CompactPacket {
            compactor: self.name().to_string(),
            rtk_version: Some(version),
            command: command_line(input),
            exit_code: input.exit_code,
            duration_ms: input.duration.as_millis(),
            failed: input.exit_code != 0,
            stdout_artifact: input.stdout_artifact.display().to_string(),
            stderr_artifact: input.stderr_artifact.display().to_string(),
            compact_artifact: None,
            raw_stdout_tokens,
            raw_stderr_tokens,
            raw_tokens: raw_stdout_tokens + raw_stderr_tokens,
            packet_tokens: estimate_tokens(compact_text.as_bytes()),
            preserved_items,
            omitted_items: vec![OmittedItem {
                source: OutputSource::Rtk,
                reason: "rtk-filtered output stored separately".to_string(),
                count: 0,
            }],
            notes,
            compact_text: Some(compact_text),
        })
    }
}

fn rtk_filter_name(input: &CompactionInput<'_>) -> Option<&'static str> {
    match (input.command, input.args.first().map(String::as_str)) {
        ("cargo", Some("test")) => Some("cargo-test"),
        ("go", Some("test")) => Some("go-test"),
        ("go", Some("build")) => Some("go-build"),
        ("pytest", _) => Some("pytest"),
        ("tsc", _) => Some("tsc"),
        ("vitest", _) => Some("vitest"),
        ("grep" | "rg", _) => Some("grep"),
        ("find" | "fd", _) => Some("find"),
        ("git", Some("log")) => Some("git-log"),
        ("git", Some("diff")) => Some("git-diff"),
        ("git", Some("status")) => Some("git-status"),
        ("log", _) => Some("log"),
        ("mypy", _) => Some("mypy"),
        ("ruff", Some("check")) => Some("ruff-check"),
        ("ruff", Some("format")) => Some("ruff-format"),
        ("prettier", _) => Some("prettier"),
        ("npx", Some("tsc" | "typescript")) => Some("tsc"),
        ("npx", Some("vitest")) => Some("vitest"),
        ("npx", Some("prettier")) => Some("prettier"),
        _ => None,
    }
}

fn collect_native_items(
    output: &[u8],
    source: OutputSource,
    preserved_items: &mut Vec<PreservedItem>,
) -> usize {
    let text = String::from_utf8_lossy(output);
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![false; lines.len()];
    let mut stack_locations = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        if let Some(location) = stack_trace_location(line)
            && !stack_locations.contains(&location)
        {
            stack_locations.push(location);
        }

        if is_error_line(line) {
            let start = index.saturating_sub(2);
            let end = (index + 5).min(lines.len().saturating_sub(1));

            for slot in keep.iter_mut().take(end + 1).skip(start) {
                *slot = true;
            }
        }
    }

    if !stack_locations.is_empty() {
        preserved_items.push(PreservedItem {
            source,
            kind: PreservedKind::StackTrace,
            line: format_stack_trace_summary(&stack_locations),
        });
    }

    for (index, line) in lines.iter().enumerate() {
        if keep[index] {
            preserved_items.push(PreservedItem {
                source,
                kind: PreservedKind::ErrorLine,
                line: line.to_string(),
            });
        }
    }

    lines.len()
}

fn omitted_items(
    stdout_lines: usize,
    stderr_lines: usize,
    preserved_items: &[PreservedItem],
) -> Vec<OmittedItem> {
    let stdout_preserved = preserved_items
        .iter()
        .filter(|item| matches!(item.source, OutputSource::Stdout))
        .count();
    let stderr_preserved = preserved_items
        .iter()
        .filter(|item| matches!(item.source, OutputSource::Stderr))
        .count();

    vec![
        OmittedItem {
            source: OutputSource::Stdout,
            reason: "non-error output omitted from compact packet".to_string(),
            count: stdout_lines.saturating_sub(stdout_preserved),
        },
        OmittedItem {
            source: OutputSource::Stderr,
            reason: "non-error output omitted from compact packet".to_string(),
            count: stderr_lines.saturating_sub(stderr_preserved),
        },
    ]
}

fn is_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("error")
        || lower.contains("failed")
        || lower.contains("failure")
        || lower.contains("panic")
        || lower.contains("assert")
        || lower.contains("assertion")
        || lower.contains("exception")
        || lower.contains("traceback")
        || lower.contains("expected")
        || lower.contains("actual")
        || line.trim_start().starts_with("left:")
        || line.trim_start().starts_with("right:")
        || contains_source_location(line)
}

fn contains_source_location(line: &str) -> bool {
    const SOURCE_EXTENSIONS: [&str; 8] = [
        ".rs:", ".py:", ".ts:", ".tsx:", ".js:", ".jsx:", ".go:", ".java:",
    ];

    SOURCE_EXTENSIONS.iter().any(|extension| {
        line.find(extension)
            .map(|index| {
                line[index + extension.len()..]
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_digit())
            })
            .unwrap_or(false)
    })
}

fn stack_trace_location(line: &str) -> Option<String> {
    rust_panic_location(line)
        .or_else(|| python_stack_location(line))
        .or_else(|| node_stack_location(line))
}

fn rust_panic_location(line: &str) -> Option<String> {
    let (_, location) = line.split_once(" panicked at ")?;
    file_line_location(location.trim())
}

fn python_stack_location(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("File ")?;
    let rest = rest.strip_prefix('"')?;
    let (path, rest) = rest.split_once('"')?;
    let rest = rest.strip_prefix(", line ")?;
    let line_number: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();

    if path.is_empty() || line_number.is_empty() {
        return None;
    }

    Some(format!("{path}:{line_number}"))
}

fn node_stack_location(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("at ") {
        return None;
    }

    let location = if let Some((_, after_open)) = trimmed.rsplit_once('(') {
        after_open.strip_suffix(')')?
    } else {
        trimmed.strip_prefix("at ")?
    };

    file_line_location(location.trim())
}

fn file_line_location(location: &str) -> Option<String> {
    let without_column = trim_trailing_number_segment(location)?;
    if trim_trailing_number_segment(without_column).is_some() {
        return Some(without_column.to_string());
    }

    if without_column.is_empty() {
        return None;
    }

    Some(location.to_string())
}

fn trim_trailing_number_segment(location: &str) -> Option<&str> {
    let (prefix, number) = location.rsplit_once(':')?;
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    Some(prefix)
}

fn format_stack_trace_summary(locations: &[String]) -> String {
    format!("Likely stack trace: - {}", locations.join(" - "))
}

fn estimate_packet_tokens(
    preserved_items: &[PreservedItem],
    omitted_items: &[OmittedItem],
    notes: &[String],
) -> usize {
    let mut text = String::new();

    for item in preserved_items {
        text.push_str(&item.line);
        text.push('\n');
    }
    for item in omitted_items {
        text.push_str(&item.reason);
        text.push('\n');
    }
    for note in notes {
        text.push_str(note);
        text.push('\n');
    }

    estimate_tokens(text.as_bytes())
}

fn command_line(input: &CompactionInput<'_>) -> String {
    if input.args.is_empty() {
        return input.command.to_string();
    }

    format!("{} {}", input.command, input.args.join(" "))
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::*;

    fn input<'a>(
        stdout: &'a [u8],
        stderr: &'a [u8],
        exit_code: i32,
        args: &'a [String],
        stdout_artifact: &'a PathBuf,
        stderr_artifact: &'a PathBuf,
    ) -> CompactionInput<'a> {
        CompactionInput {
            command: "cargo",
            args,
            exit_code,
            duration: Duration::from_millis(42),
            stdout,
            stderr,
            stdout_artifact,
            stderr_artifact,
        }
    }

    #[test]
    fn native_compactor_preserves_error_lines() {
        let mut stdout = "running tests\nthread panicked at src/main.rs:10\n".to_string();
        stdout.push_str(&"benign output line with details\n".repeat(200));
        let stderr = b"error: test failed\n  at src/lib.rs:20";
        let args = vec!["test".to_string()];
        let stdout_artifact = PathBuf::from("stdout.txt");
        let stderr_artifact = PathBuf::from("stderr.txt");
        let packet = NativeHeuristicCompactor
            .compact(&input(
                stdout.as_bytes(),
                stderr,
                101,
                &args,
                &stdout_artifact,
                &stderr_artifact,
            ))
            .expect("native compaction should work");

        assert_eq!(packet.compactor, "native");
        assert!(packet.failed);
        assert_eq!(packet.exit_code, 101);
        assert_eq!(packet.duration_ms, 42);
        assert_eq!(packet.stdout_artifact, "stdout.txt");
        assert_eq!(packet.stderr_artifact, "stderr.txt");
        assert!(packet.raw_tokens > packet.packet_tokens);
        assert!(
            packet
                .preserved_items
                .iter()
                .any(|item| item.line.contains("test failed"))
        );
        assert!(
            packet
                .preserved_items
                .iter()
                .any(|item| item.line.contains("src/lib.rs:20"))
        );
        assert!(packet.omitted_items.iter().any(|item| item.count > 0));
    }

    #[test]
    fn native_compactor_preserves_failure_lines_with_nearby_context() {
        let stdout = [
            "setup noise 1",
            "setup noise 2",
            "previous context 1",
            "previous context 2",
            "tests/auth/session.test.ts:52",
            "Expected expired token to be rejected",
            "Received success",
            "next context 1",
            "next context 2",
            "next context 3",
            "next context 4",
            "next context 5",
            "omitted tail",
        ]
        .join("\n");
        let args = vec!["test".to_string()];
        let stdout_artifact = PathBuf::from("stdout.txt");
        let stderr_artifact = PathBuf::from("stderr.txt");

        let packet = NativeHeuristicCompactor
            .compact(&input(
                stdout.as_bytes(),
                b"",
                1,
                &args,
                &stdout_artifact,
                &stderr_artifact,
            ))
            .expect("native compaction should work");

        let preserved_stdout: Vec<&str> = packet
            .preserved_items
            .iter()
            .filter(|item| matches!(item.source, OutputSource::Stdout))
            .map(|item| item.line.as_str())
            .collect();

        assert_eq!(
            preserved_stdout,
            vec![
                "previous context 1",
                "previous context 2",
                "tests/auth/session.test.ts:52",
                "Expected expired token to be rejected",
                "Received success",
                "next context 1",
                "next context 2",
                "next context 3",
                "next context 4",
            ]
        );
    }

    #[test]
    fn native_compactor_preserves_likely_stack_traces() {
        let stderr = [
            "setup noise",
            "  File \"tests/auth/session_test.py\", line 52, in test_expired_token",
            "    assert session.is_valid()",
            "thread 'main' panicked at src/auth/session.rs:117:9",
            "    at validateSession (/repo/src/auth/session.ts:117:13)",
            "    at Object.<anonymous> (/repo/tests/auth/session.test.ts:52:3)",
            "tail noise",
        ]
        .join("\n");
        let args = vec!["test".to_string()];
        let stdout_artifact = PathBuf::from("stdout.txt");
        let stderr_artifact = PathBuf::from("stderr.txt");

        let packet = NativeHeuristicCompactor
            .compact(&input(
                b"",
                stderr.as_bytes(),
                1,
                &args,
                &stdout_artifact,
                &stderr_artifact,
            ))
            .expect("native compaction should work");

        let stack_trace = packet
            .preserved_items
            .iter()
            .find(|item| matches!(item.kind, PreservedKind::StackTrace))
            .expect("stack trace summary should be preserved");

        assert_eq!(stack_trace.source, OutputSource::Stderr);
        assert_eq!(
            stack_trace.line,
            "Likely stack trace: - tests/auth/session_test.py:52 - src/auth/session.rs:117 - /repo/src/auth/session.ts:117 - /repo/tests/auth/session.test.ts:52"
        );
    }

    #[test]
    fn native_compactor_preserves_rust_assertion_details() {
        let stdout = [
            "running 1 test",
            "test commands::packet::tests::renders_packet ... FAILED",
            "",
            "failures:",
            "",
            "---- commands::packet::tests::renders_packet stdout ----",
            "thread 'commands::packet::tests::renders_packet' panicked at src/commands/packet.rs:612:9:",
            "assertion `left == right` failed",
            "  left: 1",
            " right: 2",
            "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
            "",
            "failures:",
            "    commands::packet::tests::renders_packet",
            "",
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 52 filtered out; finished in 0.00s",
        ]
        .join("\n");
        let args = vec!["test".to_string()];
        let stdout_artifact = PathBuf::from("stdout.txt");
        let stderr_artifact = PathBuf::from("stderr.txt");

        let packet = NativeHeuristicCompactor
            .compact(&input(
                stdout.as_bytes(),
                b"",
                101,
                &args,
                &stdout_artifact,
                &stderr_artifact,
            ))
            .expect("native compaction should work");

        let preserved_lines: Vec<&str> = packet
            .preserved_items
            .iter()
            .map(|item| item.line.as_str())
            .collect();

        assert!(preserved_lines.contains(&"assertion `left == right` failed"));
        assert!(preserved_lines.contains(&"  left: 1"));
        assert!(preserved_lines.contains(&" right: 2"));
        assert!(
            preserved_lines
                .iter()
                .any(|line| line.contains("src/commands/packet.rs:612"))
        );
    }

    #[test]
    fn rust_panic_location_accepts_path_line_without_column() {
        assert_eq!(
            rust_panic_location("thread 'main' panicked at src/main.rs:10"),
            Some("src/main.rs:10".to_string())
        );
    }

    #[test]
    fn native_compactor_preserves_fixture_failure_lines() {
        let cases = [
            FixtureCase {
                command: "cargo",
                args: &["test"],
                stdout: b"",
                stderr: include_bytes!("../fixtures/outputs/rust_test_failure.stderr"),
                expected_lines: &[
                    "thread 'commands::packet::tests::renders_packet' panicked at src/commands/packet.rs:612:9:",
                    "assertion `left == right` failed",
                    "  left: \"packet source\"",
                    " right: \"useful source excerpt\"",
                    "error: test failed, to rerun pass `--bin haycut`",
                ],
            },
            FixtureCase {
                command: "pytest",
                args: &[],
                stdout: include_bytes!("../fixtures/outputs/python_pytest_failure.stdout"),
                stderr: b"",
                expected_lines: &[
                    ">       assert response.status_code == 401",
                    "E       assert 200 == 401",
                    "tests/test_auth.py:27: AssertionError",
                    "FAILED tests/test_auth.py::test_expired_token_rejected - assert 200 == 401",
                ],
            },
            FixtureCase {
                command: "vitest",
                args: &[],
                stdout: include_bytes!("../fixtures/outputs/node_vitest_failure.stdout"),
                stderr: b"",
                expected_lines: &[
                    " FAIL  tests/session.test.ts > session policy > rejects expired token",
                    "AssertionError: expected true to be false // Object.is equality",
                    "- Expected",
                    "+ Received",
                    " ❯ tests/session.test.ts:44:28",
                ],
            },
        ];

        for case in cases {
            let args: Vec<String> = case.args.iter().map(|arg| (*arg).to_string()).collect();
            let stdout_artifact = PathBuf::from("stdout.txt");
            let stderr_artifact = PathBuf::from("stderr.txt");
            let input = CompactionInput {
                command: case.command,
                args: &args,
                exit_code: 1,
                duration: Duration::from_millis(42),
                stdout: case.stdout,
                stderr: case.stderr,
                stdout_artifact: &stdout_artifact,
                stderr_artifact: &stderr_artifact,
            };

            let packet = NativeHeuristicCompactor
                .compact(&input)
                .expect("native compaction should work for fixture");
            let preserved_lines: Vec<&str> = packet
                .preserved_items
                .iter()
                .map(|item| item.line.as_str())
                .collect();

            assert!(
                packet.raw_tokens > packet.packet_tokens,
                "{} fixture should produce a smaller packet: raw={}, packet={}",
                case.command,
                packet.raw_tokens,
                packet.packet_tokens
            );
            for expected_line in case.expected_lines {
                assert!(
                    preserved_lines.contains(expected_line),
                    "{} fixture did not preserve expected line: {expected_line}",
                    case.command
                );
            }
        }
    }

    #[test]
    fn token_estimator_uses_chars_divided_by_four() {
        assert_eq!(estimate_tokens(b""), 0);
        assert_eq!(estimate_tokens(b"abcd"), 1);
        assert_eq!(estimate_tokens("abcdé".as_bytes()), 1);
    }

    #[test]
    fn selects_rtk_pipe_filter_without_rerunning_command() {
        let args = vec!["test".to_string()];
        let stdout_artifact = PathBuf::from("stdout.txt");
        let stderr_artifact = PathBuf::from("stderr.txt");
        let input = input(
            b"stdout",
            b"stderr",
            101,
            &args,
            &stdout_artifact,
            &stderr_artifact,
        );

        assert_eq!(rtk_filter_name(&input), Some("cargo-test"));
    }

    #[test]
    fn selects_supported_rtk_pipe_filters() {
        let stdout_artifact = PathBuf::from("stdout.txt");
        let stderr_artifact = PathBuf::from("stderr.txt");

        let cases = [
            ("go", vec!["test"], Some("go-test")),
            ("go", vec!["build"], Some("go-build")),
            ("pytest", vec![], Some("pytest")),
            ("tsc", vec![], Some("tsc")),
            ("vitest", vec![], Some("vitest")),
            ("rg", vec!["needle"], Some("grep")),
            ("fd", vec!["main"], Some("find")),
            ("git", vec!["diff"], Some("git-diff")),
            ("git", vec!["status"], Some("git-status")),
            ("log", vec![], Some("log")),
            ("mypy", vec!["src"], Some("mypy")),
            ("ruff", vec!["check"], Some("ruff-check")),
            ("ruff", vec!["format"], Some("ruff-format")),
            ("prettier", vec!["--check", "."], Some("prettier")),
            ("npx", vec!["tsc"], Some("tsc")),
        ];

        for (command, args, expected_filter) in cases {
            let args: Vec<String> = args.into_iter().map(str::to_string).collect();
            let input = CompactionInput {
                command,
                args: &args,
                exit_code: 0,
                duration: Duration::from_millis(1),
                stdout: b"stdout",
                stderr: b"stderr",
                stdout_artifact: &stdout_artifact,
                stderr_artifact: &stderr_artifact,
            };

            assert_eq!(
                rtk_filter_name(&input),
                expected_filter,
                "{command} {args:?}"
            );
        }
    }

    struct FixtureCase {
        command: &'static str,
        args: &'static [&'static str],
        stdout: &'static [u8],
        stderr: &'static [u8],
        expected_lines: &'static [&'static str],
    }
}
