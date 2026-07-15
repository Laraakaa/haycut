/// Formats a human-readable receipt line for a quantity/price pair.
pub fn format_receipt_line(quantity: u32, unit_price_cents: i64) -> String {
    format!("{quantity} x {unit_price_cents}c")
}
