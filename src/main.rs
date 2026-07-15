mod budget;
mod cli;
mod code_graph;
mod commands;
mod compactor;
mod config;
mod context;
pub mod evidence;
pub mod extract;
pub mod model;
mod project_path;
mod store;
mod symbols;
mod util;

fn main() {
    std::process::exit(cli::run());
}
