use crate::pricing::apply_bulk_discount;

pub fn total_for(quantity: u32, unit_price_cents: i64) -> i64 {
    apply_bulk_discount(quantity, unit_price_cents)
}

#[cfg(test)]
mod tests {
    use super::total_for;

    #[test]
    fn ten_units_qualifies_for_bulk_discount() {
        assert_eq!(total_for(10, 100), 900);
    }
}
