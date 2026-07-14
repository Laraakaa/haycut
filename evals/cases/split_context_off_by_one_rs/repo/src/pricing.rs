/// Applies a 10% bulk discount once `quantity` reaches the 10-unit threshold.
pub fn apply_bulk_discount(quantity: u32, unit_price_cents: i64) -> i64 {
    let subtotal = quantity as i64 * unit_price_cents;
    if quantity > 10 {
        subtotal - subtotal / 10
    } else {
        subtotal
    }
}
