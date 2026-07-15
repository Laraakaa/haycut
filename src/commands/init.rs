use crate::config;

pub fn run(force: bool) -> i32 {
    match config::create_default_config(force) {
        Ok(()) => println!("Created haycut.toml"),
        Err(error) => {
            eprintln!("Error: {error}");
            return 1;
        }
    }

    match config::UserConfig::create_if_missing() {
        Ok(Some(path)) => println!("Created {}", path.display()),
        Ok(None) => {
            if let Some(path) = config::UserConfig::path() {
                println!("User config already exists: {}", path.display());
            }
        }
        Err(error) => {
            eprintln!("Error creating user config: {error}");
            return 1;
        }
    }

    0
}
