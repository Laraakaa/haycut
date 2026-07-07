use std::{io, path::Path};

use crate::store::{self, FileInventoryEntry, RUN_STORE_PATH};

pub const DEFAULT_LIMIT: usize = 20;

pub fn run(largest: bool, limit: usize) -> i32 {
    if !largest {
        eprintln!("Error: files currently requires --largest");
        return 2;
    }

    match load_largest_files(Path::new(RUN_STORE_PATH), limit) {
        Ok(files) => {
            print_files(&files);
            0
        }
        Err(error) => {
            eprintln!("Error loading files: {error}");
            1
        }
    }
}

fn load_largest_files(db_path: &Path, limit: usize) -> io::Result<Vec<FileInventoryEntry>> {
    store::largest_files(db_path, limit)
}

fn print_files(files: &[FileInventoryEntry]) {
    println!("{:<32}  {:>7}  {:>11}", "Path", "Lines", "Est. tokens");

    for file in files {
        println!(
            "{:<32}  {:>7}  {:>11}",
            truncate(&file.path, 32),
            format_count(file.line_count),
            format_count(file.estimated_tokens),
        );
    }
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

fn format_count(count: i64) -> String {
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
    fn formats_large_counts_with_commas() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(123), "123");
        assert_eq!(format_count(1840), "1,840");
    }

    #[test]
    fn truncates_long_paths_to_table_width() {
        assert_eq!(truncate("src/lib.rs", 12), "src/lib.rs");
        assert_eq!(truncate("src/auth/session/service.ts", 12), "src/auth/...");
    }
}
