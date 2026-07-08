mod budget;
mod cli;
mod commands;
mod compactor;
mod config;
pub mod extract;
pub mod model;
mod store;
mod symbols;
mod util;

fn main() {
    std::process::exit(cli::run());
}
