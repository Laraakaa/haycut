use crate::logging::log_order;
use crate::pricing::apply_bulk_discount;
use crate::receipt::format_receipt_line;

pub fn total_for(quantity: u32, unit_price_cents: i64) -> i64 {
    apply_bulk_discount(quantity, unit_price_cents)
}

pub fn describe_order(quantity: u32, unit_price_cents: i64) -> String {
    log_order(quantity, unit_price_cents);
    format_receipt_line(quantity, unit_price_cents)
}

#[cfg(test)]
mod tests {
    use super::{describe_order, total_for};

    #[test]
    fn ten_units_qualifies_for_bulk_discount() {
        assert_eq!(total_for(10, 100), 900);
    }

    #[test]
    fn describe_order_mentions_quantity() {
        let line = describe_order(10, 100);
        assert!(line.contains("10"));
    }
}
