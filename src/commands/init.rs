use crate::config;

pub fn run(force: bool) {
    match config::create_default_config(force) {
        Ok(()) => {
            println!("Created haycut.toml");
        }
        Err(error) => {
            eprintln!("Error: {error}");
        }
    }
}
