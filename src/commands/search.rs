use std::{io, process::Command};

use serde_json::Value;

pub const DEFAULT_LIMIT: usize = 20;

#[derive(Debug, PartialEq, Eq)]
struct SearchMatch {
    path: String,
    line_number: u64,
    line: String,
    estimated_tokens: usize,
}

pub fn run(query: Vec<String>, limit: usize) -> i32 {
    let query = query.join(" ");
    match search_exact(&query, limit) {
        Ok(matches) => {
            print_matches(&matches);
            0
        }
        Err(error) => {
            eprintln!("Error searching files: {error}");
            1
        }
    }
}

fn search_exact(query: &str, limit: usize) -> io::Result<Vec<SearchMatch>> {
    let output = Command::new("rg")
        .args([
            "--json",
            "-F",
            "--line-number",
            "--color",
            "never",
            "--",
            query,
            ".",
        ])
        .output()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "rg is required for search but was not found",
                )
            } else {
                error
            }
        })?;

    if !output.status.success() && output.status.code() != Some(1) {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    parse_rg_matches(&output.stdout, limit)
}

fn parse_rg_matches(output: &[u8], limit: usize) -> io::Result<Vec<SearchMatch>> {
    let mut matches = Vec::new();
    for line in String::from_utf8_lossy(output).lines() {
        if line.trim().is_empty() {
            continue;
        }

        if matches.len() >= limit {
            break;
        }

        let event: Value = serde_json::from_str(line).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid rg JSON: {error}"),
            )
        })?;

        if event.get("type").and_then(Value::as_str) != Some("match") {
            continue;
        }

        let data = event.get("data").ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "rg match event missing data")
        })?;
        let path = text_field(data, &["path", "text"])?;
        let line_text = text_field(data, &["lines", "text"])?;
        let line_number = data
            .get("line_number")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "rg match event missing line number",
                )
            })?;
        let line = line_text.trim_end_matches(['\r', '\n']).to_string();

        matches.push(SearchMatch {
            path: path.to_string(),
            line_number,
            estimated_tokens: estimate_tokens(&line),
            line,
        });
    }

    Ok(matches)
}

fn text_field<'a>(value: &'a Value, path: &[&str]) -> io::Result<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("rg match event missing {}", path.join(".")),
            )
        })?;
    }

    current.as_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("rg match event field {} is not text", path.join(".")),
        )
    })
}

fn print_matches(matches: &[SearchMatch]) {
    println!("{:<36}  {:>6}  {:>11}  Text", "Path", "Line", "Est. tokens");

    for item in matches {
        println!(
            "{:<36}  {:>6}  {:>11}  {}",
            truncate(&item.path, 36),
            item.line_number,
            format_count(item.estimated_tokens),
            truncate(&item.line, 80),
        );
    }

    let total_tokens: usize = matches.iter().map(|item| item.estimated_tokens).sum();
    println!("Estimated token cost: {}", format_count(total_tokens));
}

fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / 4
}

fn truncate(value: &str, max_width: usize) -> String {
    if value.chars().count() <= max_width {
        return value.to_string();
    }

    let mut truncated = value
        .chars()
        .take(max_width.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
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
    use super::*;

    #[test]
    fn parses_rg_json_matches_and_applies_limit() {
        let output = br#"
{"type":"begin","data":{"path":{"text":"src/lib.rs"}}}
{"type":"match","data":{"path":{"text":"src/lib.rs"},"lines":{"text":"fn validateSession() {}\n"},"line_number":7,"absolute_offset":0,"submatches":[]}}
{"type":"match","data":{"path":{"text":"src/main.rs"},"lines":{"text":"validateSession();\n"},"line_number":3,"absolute_offset":0,"submatches":[]}}
"#;

        let matches = parse_rg_matches(output, 1).expect("rg matches should parse");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, "src/lib.rs");
        assert_eq!(matches[0].line_number, 7);
        assert_eq!(matches[0].line, "fn validateSession() {}");
        assert_eq!(matches[0].estimated_tokens, 5);
    }

    #[test]
    fn formats_large_counts_with_commas() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(1840), "1,840");
    }
}
