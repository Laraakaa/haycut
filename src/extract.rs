/// Structured file reference extracted from compacted or raw diagnostic output.
///
/// Supports these formats (HC-042):
///   - `-->` prefix:         `--> src/config.rs:54:9`   (Rust compiler)
///   - General path:col:     `src/lib.rs:12:5`
///   - Path:line only:       `tests/test_config.py:42`
///   - Python traceback:     `File "...", line 123`
use serde::Serialize;

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
        let column = if after_line.starts_with(':') {
            let col_str: String = after_line[1..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
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
}
