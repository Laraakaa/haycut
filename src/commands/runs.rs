use std::{io, path::Path};

use crate::store::{self, RUN_STORE_PATH, RunSummary};

pub const DEFAULT_LIMIT: usize = 20;

pub fn run(limit: usize) -> i32 {
    match load_runs(Path::new(RUN_STORE_PATH), limit) {
        Ok(runs) => {
            print_runs(&runs);
            0
        }
        Err(error) => {
            eprintln!("Error listing runs: {error}");
            1
        }
    }
}

fn load_runs(db_path: &Path, limit: usize) -> io::Result<Vec<RunSummary>> {
    store::recent_runs(db_path, limit)
}

fn print_runs(runs: &[RunSummary]) {
    println!(
        "{:<28}  {:<28}  {:>4}  {:>8}  {:>10}  {:>9}",
        "ID", "Command", "Exit", "Raw tok", "Packet tok", "Reduction"
    );

    for run in runs {
        let raw_tokens = run.raw_tokens.unwrap_or(0);
        let packet_tokens = run.packet_tokens.unwrap_or(0);
        println!(
            "{:<28}  {:<28}  {:>4}  {:>8}  {:>10}  {:>9}",
            truncate(&run.id, 28),
            truncate(&run.command, 28),
            format_optional(run.exit_code),
            format_optional(run.raw_tokens),
            format_optional(run.packet_tokens),
            format!("{:.1}%", reduction_percent(raw_tokens, packet_tokens)),
        );
    }
}

fn reduction_percent(raw_tokens: i64, packet_tokens: i64) -> f64 {
    if raw_tokens <= 0 {
        return 0.0;
    }

    let reduced_tokens = raw_tokens.saturating_sub(packet_tokens).max(0);
    reduced_tokens as f64 / raw_tokens as f64 * 100.0
}

fn format_optional<T>(value: Option<T>) -> String
where
    T: ToString,
{
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_reduction_percentage() {
        assert_eq!(reduction_percent(0, 10), 0.0);
        assert_eq!(reduction_percent(100, 25), 75.0);
        assert_eq!(reduction_percent(100, 125), 0.0);
    }

    #[test]
    fn truncates_long_values_to_table_width() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("123456789", 5), "12...");
    }
}
