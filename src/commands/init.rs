use crate::config;

pub fn run(force: bool) {
    match config::create_default_config(force) {
        Ok(()) => println!("Created haycut.toml"),
        Err(error) => eprintln!("Error: {error}"),
    }

    match config::UserConfig::create_if_missing() {
        Ok(Some(path)) => println!("Created {}", path.display()),
        Ok(None) => {
            if let Some(path) = config::UserConfig::path() {
                println!("User config already exists: {}", path.display());
            }
        }
        Err(error) => eprintln!("Warning: could not create user config: {error}"),
    }
}
