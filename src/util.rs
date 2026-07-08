/// Rough token estimate: one token per four characters of UTF-8 output.
///
/// This is the single canonical estimate used across all HayCut commands.
pub fn estimate_tokens(output: &[u8]) -> usize {
    String::from_utf8_lossy(output).chars().count() / 4
}

/// Format an integer with comma thousand-separators, e.g. `1,234,567`.
pub fn format_count(count: usize) -> String {
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
    fn estimates_tokens_by_char_count() {
        assert_eq!(estimate_tokens(b"abcd"), 1);
        assert_eq!(estimate_tokens(b""), 0);
        assert_eq!(estimate_tokens(b"0123456789abcdef"), 4);
    }

    #[test]
    fn formats_counts_with_commas() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }
}
