mod budget;
mod cli;
mod commands;
mod compactor;
mod config;
mod store;
mod symbols;

fn main() {
    std::process::exit(cli::run());
}
