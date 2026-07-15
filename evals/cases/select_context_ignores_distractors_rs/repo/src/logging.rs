/// Logs order details once quantity passes the promotional threshold.
/// (Despite the name, this never touches pricing — it only formats a message.)
pub fn log_order(quantity: u32, unit_price_cents: i64) -> String {
    format!("order: {quantity} units @ {unit_price_cents} cents each")
}
