use std::{fs, io, path::PathBuf};

use crate::util::estimate_tokens;

pub const DEFAULT_RADIUS: usize = 20;
const MAX_WINDOW_LINES: usize = 400;

pub fn run(path: PathBuf, line: usize, radius: usize, force: bool) -> i32 {
    match read_window(path, line, radius, force) {
        Ok(window) => {
            print!("{}", window.render());
            0
        }
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            eprintln!("Error: {error}");
            2
        }
        Err(error) => {
            eprintln!("Error reading window: {error}");
            1
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FileWindow {
    path: PathBuf,
    start_line: usize,
    end_line: usize,
    token_estimate: usize,
    lines: Vec<NumberedLine>,
}

#[derive(Debug, PartialEq, Eq)]
struct NumberedLine {
    number: usize,
    text: String,
}

impl FileWindow {
    fn render(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!("File: {}\n", self.path.display()));
        output.push_str(&format!("Lines: {}-{}\n", self.start_line, self.end_line));
        output.push_str(&format!("Estimated tokens: {}\n", self.token_estimate));
        output.push_str("<code>\n");
        for line in &self.lines {
            output.push_str(&format!("{:>5} | {}\n", line.number, line.text));
        }
        output.push_str("</code>\n");

        output
    }
}

fn read_window(path: PathBuf, line: usize, radius: usize, force: bool) -> io::Result<FileWindow> {
    if line == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--line must be greater than 0",
        ));
    }

    let requested_lines = requested_window_lines(radius)?;
    if requested_lines > MAX_WINDOW_LINES && !force {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "window would read {requested_lines} lines; use --force to exceed {MAX_WINDOW_LINES} lines"
            ),
        ));
    }

    let source = fs::read_to_string(&path)?;
    let source_lines: Vec<&str> = source.lines().collect();
    if source_lines.is_empty() {
        return Ok(FileWindow {
            path,
            start_line: 0,
            end_line: 0,
            token_estimate: 0,
            lines: Vec::new(),
        });
    }

    let start_line = line.saturating_sub(radius).max(1);
    let end_line = line.saturating_add(radius).min(source_lines.len());
    let lines: Vec<NumberedLine> = source_lines[start_line - 1..end_line]
        .iter()
        .enumerate()
        .map(|(offset, text)| NumberedLine {
            number: start_line + offset,
            text: (*text).to_string(),
        })
        .collect();
    let excerpt = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    Ok(FileWindow {
        path,
        start_line,
        end_line,
        token_estimate: estimate_tokens(excerpt.as_bytes()),
        lines,
    })
}

fn requested_window_lines(radius: usize) -> io::Result<usize> {
    radius
        .checked_mul(2)
        .and_then(|lines| lines.checked_add(1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "window size is too large"))
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn reads_requested_line_window_with_numbers_and_token_estimate() {
        let path = write_numbered_file("read-window-range", 150);

        let window = read_window(path.clone(), 117, 20, false).expect("window should read");
        let rendered = window.render();

        assert_eq!(window.start_line, 97);
        assert_eq!(window.end_line, 137);
        assert_eq!(window.lines.len(), 41);
        assert!(window.token_estimate > 0);
        assert!(rendered.contains("Lines: 97-137"));
        assert!(rendered.contains("Estimated tokens:"));
        assert!(rendered.contains("   97 | line 97"));
        assert!(rendered.contains("  137 | line 137"));

        fs::remove_file(path).expect("test file should be removed");
    }

    #[test]
    fn clamps_window_to_file_bounds() {
        let path = write_numbered_file("read-window-clamp", 5);

        let window = read_window(path.clone(), 2, 20, false).expect("window should read");

        assert_eq!(window.start_line, 1);
        assert_eq!(window.end_line, 5);
        assert_eq!(window.lines.len(), 5);

        fs::remove_file(path).expect("test file should be removed");
    }

    #[test]
    fn refuses_extremely_large_windows_without_force() {
        let path = write_numbered_file("read-window-large", 1000);

        let error = read_window(path.clone(), 500, 250, false)
            .expect_err("large window should require force");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("use --force"));

        fs::remove_file(path).expect("test file should be removed");
    }

    #[test]
    fn allows_extremely_large_windows_with_force() {
        let path = write_numbered_file("read-window-force", 1000);

        let window = read_window(path.clone(), 500, 250, true).expect("forced window should read");

        assert_eq!(window.start_line, 250);
        assert_eq!(window.end_line, 750);
        assert_eq!(window.lines.len(), 501);

        fs::remove_file(path).expect("test file should be removed");
    }

    fn write_numbered_file(label: &str, line_count: usize) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "haycut-{label}-{}-{}.txt",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos()
        ));
        let contents = (1..=line_count)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, contents).expect("test file should be written");

        path
    }
}
