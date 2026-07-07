mod cli;
mod commands;
mod config;

fn main() {
    std::process::exit(cli::run());
}
