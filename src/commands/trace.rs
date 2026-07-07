use crate::config::Config;

pub fn run() {
    match Config::load_from_current_dir() {
        Ok(config) => {
            println!("Tracing with config:");
            println!("soft budget: {}", config.token.soft_budget);
            println!("hard budget: {}", config.token.hard_budget);
            println!("max output bytes: {}", config.trace.max_output_bytes);
            println!("store full output: {}", config.trace.store_full_output);
        }
        Err(error) => {
            eprintln!("Error loading config: {error}");
        }
    }
}
