use std::path::Path;

/// The single structured representation used when a presenter renders source
/// code. `semantic_label` is intended for non-editable diagnostic sections.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CodeContext<'a> {
    pub symbol: Option<&'a str>,
    pub path: Option<&'a str>,
    pub start_line: Option<usize>,
    pub source: &'a str,
    pub semantic_label: Option<&'a str>,
}

/// Render a source snippet with the canonical location header and language
/// fence. Non-code observations do not use this API.
pub(crate) fn render_code_context(context: CodeContext<'_>) -> String {
    let mut output = String::new();
    if let Some(label) = context.semantic_label {
        output.push_str(label);
        output.push_str(":\n");
    }

    let symbol = context.symbol.unwrap_or("context");
    let path = context
        .path
        .map(normalize_path)
        .unwrap_or_else(|| "<unknown>".to_string());
    let line = context.start_line.unwrap_or(1);
    output.push_str(&format!("{symbol}@{path}:{line}\n"));
    output.push_str(&format!("```{}\n", language_for_path(&path)));
    output.push_str(context.source.trim_end_matches('\n'));
    output.push_str("\n```\n");
    output
}

fn normalize_path(path: &str) -> String {
    let path = Path::new(path);
    let relative = std::env::current_dir()
        .ok()
        .and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path);
    let value = relative.to_string_lossy().replace('\\', "/");
    value.strip_prefix("./").unwrap_or(&value).to_string()
}

fn language_for_path(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("ts") => "typescript",
        Some("tsx") => "tsx",
        Some("js") | Some("jsx") => "javascript",
        Some("go") => "go",
        Some("java") => "java",
        Some("rb") => "ruby",
        Some("sh") | Some("bash") => "bash",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml") | Some("yml") => "yaml",
        _ => "text",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(path: &str) -> String {
        render_code_context(CodeContext {
            symbol: Some("example"),
            path: Some(path),
            start_line: Some(7),
            source: "return value;\n",
            semantic_label: None,
        })
    }

    #[test]
    fn renders_rust_with_canonical_location_and_fence() {
        let output = render("src/example.rs");
        assert!(output.contains("example@src/example.rs:7\n```rust\n"));
    }

    #[test]
    fn renders_python_and_typescript_languages() {
        assert!(render("src/example.py").contains("```python\n"));
        assert!(render("src/example.ts").contains("```typescript\n"));
    }

    #[test]
    fn falls_back_to_text_for_unknown_extensions() {
        assert!(render("src/example.weird").contains("```text\n"));
    }

    #[test]
    fn renders_diagnostic_label_once_per_context() {
        let output = render_code_context(CodeContext {
            symbol: Some("test_total"),
            path: Some("src/cart.rs"),
            start_line: Some(19),
            source: "assert_eq!(total_for(10, 100), 900);",
            semantic_label: Some("Diagnostic failure path (not an edit target)"),
        });
        assert_eq!(
            output
                .matches("Diagnostic failure path (not an edit target):")
                .count(),
            1
        );
    }
}
