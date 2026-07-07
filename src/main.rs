mod cli;
mod commands;
mod compactor;
mod config;

fn main() {
    std::process::exit(cli::run());
}
