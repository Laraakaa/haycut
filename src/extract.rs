/// Structured file reference extracted from compacted or raw diagnostic output.
///
/// Supports these formats (HC-042):
///   - `-->` prefix:         `--> src/config.rs:54:9`   (Rust compiler)
///   - General path:col:     `src/lib.rs:12:5`
///   - Path:line only:       `tests/test_config.py:42`
///   - Python traceback:     `File "...", line 123`
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileReference {
    pub path: String,
    pub line: usize,
    /// Column number, if present in the source text.
    pub column: Option<usize>,
    /// Which output stream the reference was found in (e.g. "stderr").
    pub source: String,
    /// Human-readable description of why this location was extracted.
    pub reason: &'static str,
}

/// Severity of an extracted [`Diagnostic`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// Category of an extracted [`Diagnostic`], from most to least specific.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    RustCompileError,
    TestFailure,
    Panic,
    Generic,
}

/// A structured diagnostic extracted from raw command output.
///
/// This is the semantic unit HayCut derives evidence from. Extraction runs in
/// layers (see [`extract_diagnostics`]): a Rust compiler layer, a test-failure
/// layer, and a generic fallback layer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    pub severity: Severity,
    pub code: Option<String>,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    /// Which output stream the diagnostic was found in (e.g. "stderr").
    pub source: String,
    /// Test name for [`DiagnosticKind::TestFailure`] diagnostics.
    pub test_name: Option<String>,
}

/// Extract structured diagnostics from `stdout` and `stderr`.
///
/// Runs three layers in priority order:
/// 1. Rust compiler diagnostics (`error[E0063]: ...` + `--> file:line:col`).
/// 2. Test failures (`test NAME ... FAILED`, `panicked at file:line:col`).
/// 3. Generic fallback (obvious error/warning/panic lines) — only when the
///    first two layers found nothing, keeping structured output clean.
///
/// `stderr` is scanned before `stdout` because failures usually surface there.
pub fn extract_diagnostics(stdout: &str, stderr: &str, _exit_code: i32) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for (text, source) in [(stderr, "stderr"), (stdout, "stdout")] {
        diagnostics.extend(rust_compiler_diagnostics(text, source));
        diagnostics.extend(test_failure_diagnostics(text, source));
    }

    if diagnostics.is_empty() {
        for (text, source) in [(stderr, "stderr"), (stdout, "stdout")] {
            diagnostics.extend(generic_diagnostics(text, source));
        }
    }

    dedupe_diagnostics(diagnostics)
}

// ── Rust compiler layer ───────────────────────────────────────────────────────

/// Detect `error[E0063]: message` / `warning: message` headers and attach the
/// location from the following `--> file:line:col` arrow line.
fn rust_compiler_diagnostics(text: &str, source: &str) -> Vec<Diagnostic> {
    let lines: Vec<&str> = text.lines().collect();
    let mut diagnostics = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let Some((severity, code, message)) = parse_rust_header(line) else {
            continue;
        };

        // Look ahead a few lines for the `--> file:line:col` arrow, stopping if
        // another header begins first.
        let mut file = None;
        let mut line_number = None;
        let mut column = None;
        for look in lines.iter().skip(index + 1).take(4) {
            if parse_rust_header(look).is_some() {
                break;
            }
            if let Some(location) = rust_compiler_location(look, source) {
                file = Some(location.path);
                line_number = Some(location.line);
                column = location.column;
                break;
            }
        }

        // Only treat as a compile diagnostic when it carries a code or a
        // resolved location; this excludes cargo lines like
        // `error: could not compile ...`.
        if code.is_none() && file.is_none() {
            continue;
        }

        diagnostics.push(Diagnostic {
            kind: DiagnosticKind::RustCompileError,
            severity,
            code,
            message,
            file,
            line: line_number,
            column,
            source: source.to_string(),
            test_name: None,
        });
    }

    diagnostics
}

/// Parse a Rust compiler header line: `error[E0063]: msg`, `warning: msg`,
/// or `error TS2322: msg`. Returns `(severity, code, message)`.
fn parse_rust_header(line: &str) -> Option<(Severity, Option<String>, String)> {
    let trimmed = line.trim_start();
    let (severity, rest) = if let Some(rest) = trimmed.strip_prefix("error") {
        (Severity::Error, rest)
    } else if let Some(rest) = trimmed.strip_prefix("warning") {
        (Severity::Warning, rest)
    } else {
        return None;
    };

    // Optional bracketed code: `[E0063]`.
    let (code, after_code) = if let Some(rest) = rest.strip_prefix('[') {
        let end = rest.find(']')?;
        (Some(rest[..end].to_string()), &rest[end + 1..])
    } else {
        (None, rest)
    };

    let message = after_code.strip_prefix(':')?.trim().to_string();
    if message.is_empty() {
        return None;
    }

    Some((severity, code, message))
}

// ── test-failure layer ────────────────────────────────────────────────────────

/// Detect `test NAME ... FAILED` summary lines and `panicked at file:line:col`
/// locations (with the assertion/message from the following line).
fn test_failure_diagnostics(text: &str, source: &str) -> Vec<Diagnostic> {
    let lines: Vec<&str> = text.lines().collect();
    let mut diagnostics = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if let Some(test_name) = parse_failed_test(trimmed) {
            diagnostics.push(Diagnostic {
                kind: DiagnosticKind::TestFailure,
                severity: Severity::Error,
                code: None,
                message: format!("test {test_name} failed"),
                file: None,
                line: None,
                column: None,
                source: source.to_string(),
                test_name: Some(test_name),
            });
            continue;
        }

        if let Some((test_name, location)) = parse_panic(trimmed, source) {
            let base_message = lines
                .get(index + 1)
                .map(|next| next.trim())
                .filter(|next| !next.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| "panicked".to_string());
            let message = if is_assertion_message(&base_message) {
                let diff = collect_assertion_diff(&lines, index + 2);
                if diff.is_empty() {
                    base_message
                } else {
                    format!("{base_message}\n{}", diff.join("\n"))
                }
            } else {
                base_message
            };

            diagnostics.push(Diagnostic {
                kind: DiagnosticKind::Panic,
                severity: Severity::Error,
                code: None,
                message,
                file: location.as_ref().map(|l| l.path.clone()),
                line: location.as_ref().map(|l| l.line),
                column: location.as_ref().and_then(|l| l.column),
                source: source.to_string(),
                test_name,
            });
        }
    }

    diagnostics
}

/// Parse `test some::path ... FAILED`, returning the test name.
fn parse_failed_test(line: &str) -> Option<String> {
    let rest = line.strip_prefix("test ")?;
    let name = rest.strip_suffix(" ... FAILED")?.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// Parse `thread 'NAME' panicked at file:line:col:`, returning the thread/test
/// name and the panic location (if the `panicked at` clause carries one).
fn parse_panic(line: &str, source: &str) -> Option<(Option<String>, Option<FileReference>)> {
    let panic_index = line.find("panicked at")?;

    // Thread name lives in the first `'...'` before the panic clause.
    let head = &line[..panic_index];
    let test_name = head.find('\'').and_then(|start| {
        let inner = &head[start + 1..];
        inner.find('\'').map(|end| inner[..end].to_string())
    });

    let after_panic = line[panic_index + "panicked at".len()..].trim();
    let location = parse_path_line_col(after_panic, source, "panic location");

    Some((test_name, location))
}

/// True if `message` looks like a Rust assertion failure (as opposed to a
/// bare `panicked` or custom `panic!(...)` message).
fn is_assertion_message(message: &str) -> bool {
    message.starts_with("assertion")
}

/// Collect the indented `left:` / `right:` (or `expected:` / `actual:`)
/// detail lines that follow an assertion failure message, starting at
/// `start_index`. Stops at the first blank or non-detail line.
fn collect_assertion_diff(lines: &[&str], start_index: usize) -> Vec<String> {
    let mut detail = Vec::new();

    for line in lines.iter().skip(start_index) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        let is_detail = trimmed.starts_with("left:")
            || trimmed.starts_with("right:")
            || trimmed.starts_with("expected:")
            || trimmed.starts_with("actual:");
        if !is_detail {
            break;
        }
        detail.push(trimmed.to_string());
    }

    detail
}

// ── generic fallback layer ────────────────────────────────────────────────────

/// Emit a generic diagnostic for each obvious error/warning/panic/assertion
/// line. Used only as a fallback when no structured diagnostics were found.
fn generic_diagnostics(text: &str, source: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let lower = trimmed.to_ascii_lowercase();
        let is_error = lower.starts_with("error")
            || lower.contains("panic")
            || lower.contains("assertion")
            || lower.contains("failed")
            || lower.contains("exception")
            || lower.contains("traceback");
        let is_warning = lower.starts_with("warning");

        if !is_error && !is_warning {
            continue;
        }

        diagnostics.push(Diagnostic {
            kind: DiagnosticKind::Generic,
            severity: if is_warning {
                Severity::Warning
            } else {
                Severity::Error
            },
            code: None,
            message: trimmed.to_string(),
            file: None,
            line: None,
            column: None,
            source: source.to_string(),
            test_name: None,
        });
    }

    diagnostics
}

/// Drop diagnostics that duplicate an earlier one on kind, message, and file.
fn dedupe_diagnostics(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let mut seen = Vec::new();
    let mut unique = Vec::new();

    for diagnostic in diagnostics {
        let key = (
            diagnostic.kind,
            diagnostic.message.clone(),
            diagnostic.file.clone(),
            diagnostic.line,
        );
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        unique.push(diagnostic);
    }

    unique
}

/// Extract every file reference from `text`, tagging each one with `source`
/// (e.g. `"stderr"` or `"stdout"`).
///
/// Patterns tried per line, in order:
/// 1. Rust `-->` compiler diagnostic: `--> path:line:col`
/// 2. Python traceback: `File "path", line N`
/// 3. General `path:line` or `path:line:col`
pub fn extract_file_references(text: &str, source: &str) -> Vec<FileReference> {
    let mut refs: Vec<FileReference> = Vec::new();

    for line in text.lines() {
        if let Some(r) = rust_compiler_location(line, source) {
            push_unique(r, &mut refs);
            continue;
        }

        if let Some(r) = python_traceback_location(line, source) {
            push_unique(r, &mut refs);
            continue;
        }

        for r in general_colon_locations(line, source) {
            push_unique(r, &mut refs);
        }
    }

    refs
}

// ── per-pattern parsers ───────────────────────────────────────────────────────

/// Handles `--> src/config.rs:54:9` (Rust compiler diagnostic arrow).
fn rust_compiler_location(line: &str, source: &str) -> Option<FileReference> {
    let rest = line.trim_start().strip_prefix("-->")?;
    let token = rest.trim();
    parse_path_line_col(token, source, "compiler diagnostic location")
}

/// Handles `File "path/to/file.py", line 42` (Python traceback).
fn python_traceback_location(line: &str, source: &str) -> Option<FileReference> {
    // Allow arbitrary leading whitespace (indented in a traceback).
    let trimmed = line.trim_start();
    let after_file = trimmed.strip_prefix("File ")?;
    let quote = after_file.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }

    let inner = &after_file[quote.len_utf8()..];
    let path_end = inner.find(quote)?;
    let path = &inner[..path_end];
    let after_quote = &inner[path_end + quote.len_utf8()..];
    let line_marker = ", line ";
    let line_str_start = after_quote.find(line_marker)? + line_marker.len();
    let line_number: usize = after_quote[line_str_start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()?;

    if line_number == 0 || path.is_empty() || !looks_like_source_path(path) {
        return None;
    }

    Some(FileReference {
        path: strip_leading_dot_slash(path),
        line: line_number,
        column: None,
        source: source.to_string(),
        reason: "python traceback location",
    })
}

/// Handles `path:line`, `path:line:col`, and the common
/// `tests/foo.py:42: message` form. Returns all non-overlapping matches on
/// the line.
fn general_colon_locations(line: &str, source: &str) -> Vec<FileReference> {
    let mut results = Vec::new();

    // Walk char-by-char looking for potential path starts.
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut skip_until = 0;

    for i in 0..chars.len() {
        let (byte_pos, ch) = chars[i];
        if byte_pos < skip_until {
            continue;
        }

        if !is_path_start(ch) {
            continue;
        }

        // Require a word boundary before this position.
        if i > 0 {
            let (_, prev) = chars[i - 1];
            if is_path_char(prev) {
                continue;
            }
        }

        let slice = &line[byte_pos..];
        if let Some(file_ref) = parse_path_line_col(slice, source, classify_general(slice)) {
            skip_until = byte_pos + file_ref.path.len() + 1 /* colon */;
            results.push(file_ref);
        }
    }

    results
}

// ── shared helpers ────────────────────────────────────────────────────────────

/// Parse `path:line[:col]` from the start of `text`.
fn parse_path_line_col(text: &str, source: &str, reason: &'static str) -> Option<FileReference> {
    // Find the first `:` that separates a valid source path from a line number.
    for (index, ch) in text.char_indices() {
        if ch != ':' {
            continue;
        }

        let path = &text[..index];
        if !looks_like_source_path(path) {
            continue;
        }

        let after_colon = &text[index + 1..];
        let line_str: String = after_colon
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let line: usize = line_str.parse().ok()?;
        if line == 0 {
            continue;
        }

        // Optional column: `:N` immediately after the line number.
        let after_line = &after_colon[line_str.len()..];
        let column = if let Some(rest) = after_line.strip_prefix(':') {
            let col_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            col_str.parse::<usize>().ok().filter(|&c| c > 0)
        } else {
            None
        };

        return Some(FileReference {
            path: strip_leading_dot_slash(path),
            line,
            column,
            source: source.to_string(),
            reason,
        });
    }

    None
}

fn classify_general(text: &str) -> &'static str {
    // Identify the Rust compiler `-->` has already been handled above; here we
    // distinguish test-style assertion locations from plain file references.
    if text.contains("test") || text.contains("spec") {
        "test file location"
    } else {
        "source file location"
    }
}

fn looks_like_source_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with("http://") || path.starts_with("https://") {
        return false;
    }

    if !path.chars().all(is_path_char) {
        return false;
    }

    // Must look like a relative or absolute file path with a known extension,
    // or contain a directory separator.
    path.contains('/')
        || path.ends_with(".rs")
        || path.ends_with(".py")
        || path.ends_with(".ts")
        || path.ends_with(".tsx")
        || path.ends_with(".js")
        || path.ends_with(".jsx")
        || path.ends_with(".go")
        || path.ends_with(".java")
}

fn is_path_start(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '/')
}

fn is_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/')
}

fn strip_leading_dot_slash(path: &str) -> String {
    path.trim_start_matches("./").to_string()
}

fn push_unique(r: FileReference, refs: &mut Vec<FileReference>) {
    if !refs.iter().any(|e| e.path == r.path && e.line == r.line) {
        refs.push(r);
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(text: &str) -> Vec<FileReference> {
        extract_file_references(text, "stderr")
    }

    // ── HC-042 acceptance criteria ────────────────────────────────────────────

    #[test]
    fn extracts_rust_compiler_arrow() {
        let result = refs("   --> src/config.rs:54:9");
        assert_eq!(result.len(), 1);
        let r = &result[0];
        assert_eq!(r.path, "src/config.rs");
        assert_eq!(r.line, 54);
        assert_eq!(r.column, Some(9));
        assert_eq!(r.source, "stderr");
        assert_eq!(r.reason, "compiler diagnostic location");
    }

    #[test]
    fn extracts_general_path_with_line_and_column() {
        let result = refs("src/lib.rs:12:5");
        assert_eq!(result.len(), 1);
        let r = &result[0];
        assert_eq!(r.path, "src/lib.rs");
        assert_eq!(r.line, 12);
        assert_eq!(r.column, Some(5));
    }

    #[test]
    fn extracts_python_pytest_path_line_only() {
        let result = refs("tests/test_config.py:42");
        assert_eq!(result.len(), 1);
        let r = &result[0];
        assert_eq!(r.path, "tests/test_config.py");
        assert_eq!(r.line, 42);
        assert_eq!(r.column, None);
    }

    #[test]
    fn extracts_python_traceback_file_line() {
        let result = refs(r#"  File "tests/test_config.py", line 123"#);
        assert_eq!(result.len(), 1);
        let r = &result[0];
        assert_eq!(r.path, "tests/test_config.py");
        assert_eq!(r.line, 123);
        assert_eq!(r.column, None);
        assert_eq!(r.reason, "python traceback location");
    }

    // ── additional cases ──────────────────────────────────────────────────────

    #[test]
    fn deduplicates_same_path_and_line() {
        let text = "src/lib.rs:10:1\nsrc/lib.rs:10:5";
        let result = refs(text);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, "src/lib.rs");
        assert_eq!(result[0].line, 10);
    }

    #[test]
    fn rust_arrow_wins_over_general_for_same_line() {
        // When both patterns could match the same location, the arrow takes priority.
        let result = refs("--> src/config.rs:54:9");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason, "compiler diagnostic location");
    }

    #[test]
    fn ignores_urls() {
        let result = refs("see https://doc.rust-lang.org/std/index.html for info");
        assert!(result.is_empty());
    }

    #[test]
    fn extracts_source_field() {
        let result = extract_file_references("src/main.rs:5", "stdout");
        assert_eq!(result[0].source, "stdout");
    }

    #[test]
    fn ignores_lines_without_source_paths() {
        let result = refs("FAILED tests::my_test");
        assert!(result.is_empty());
    }

    // ── diagnostic extraction ─────────────────────────────────────────────────

    #[test]
    fn extracts_rust_compile_error_with_location() {
        let text = "error[E0063]: missing field `model` in initializer of `config::Config`\n  --> src/config.rs:54:9";
        let diagnostics = extract_diagnostics("", text, 101);
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic.kind, DiagnosticKind::RustCompileError);
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.code.as_deref(), Some("E0063"));
        assert_eq!(
            diagnostic.message,
            "missing field `model` in initializer of `config::Config`"
        );
        assert_eq!(diagnostic.file.as_deref(), Some("src/config.rs"));
        assert_eq!(diagnostic.line, Some(54));
        assert_eq!(diagnostic.column, Some(9));
        assert_eq!(diagnostic.source, "stderr");
    }

    #[test]
    fn ignores_cargo_could_not_compile_line() {
        let text = "error: could not compile `haycut` (bin \"haycut\") due to 1 previous error";
        let diagnostics = extract_diagnostics("", text, 101);
        assert!(
            diagnostics
                .iter()
                .all(|d| d.kind != DiagnosticKind::RustCompileError)
        );
    }

    #[test]
    fn extracts_failed_test_and_panic_location() {
        let text = "test commands::packet::tests::renders_packet ... FAILED\n\
             thread 'commands::packet::tests::renders_packet' panicked at src/commands/packet.rs:612:9:\n\
             assertion `left == right` failed";
        let diagnostics = extract_diagnostics("", text, 101);

        let failed = diagnostics
            .iter()
            .find(|d| d.kind == DiagnosticKind::TestFailure)
            .expect("test failure diagnostic");
        assert_eq!(
            failed.test_name.as_deref(),
            Some("commands::packet::tests::renders_packet")
        );

        let panic = diagnostics
            .iter()
            .find(|d| d.kind == DiagnosticKind::Panic)
            .expect("panic diagnostic");
        assert_eq!(panic.file.as_deref(), Some("src/commands/packet.rs"));
        assert_eq!(panic.line, Some(612));
        assert_eq!(panic.column, Some(9));
        assert_eq!(panic.message, "assertion `left == right` failed");
    }

    #[test]
    fn extracts_assertion_diff_into_panic_message() {
        let text = "test commands::pricing::tests::ten_units_qualifies_for_bulk_discount ... FAILED\n\
             thread 'commands::pricing::tests::ten_units_qualifies_for_bulk_discount' panicked at src/cart.rs:42:9:\n\
             assertion `left == right` failed\n\
             \x20 left: 1000\n\
             \x20 right: 900\n\
             note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace";
        let diagnostics = extract_diagnostics("", text, 101);

        let panic = diagnostics
            .iter()
            .find(|d| d.kind == DiagnosticKind::Panic)
            .expect("panic diagnostic");
        assert_eq!(
            panic.message,
            "assertion `left == right` failed\nleft: 1000\nright: 900"
        );
    }

    #[test]
    fn generic_layer_runs_only_as_fallback() {
        let text = "error: something broke\nnote: irrelevant";
        let diagnostics = extract_diagnostics("", text, 1);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].kind, DiagnosticKind::Generic);
        assert_eq!(diagnostics[0].message, "error: something broke");
    }
}
