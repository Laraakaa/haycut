pub fn sum(a: i64, b: i64) -> i64 { a + b }

#[cfg(test)]
mod tests {
    use super::sum;
    #[test]
    fn sums_two_numbers() {
        assert_eq!(sum(2, 2), 5); // deliberately wrong expected value
    }
}
