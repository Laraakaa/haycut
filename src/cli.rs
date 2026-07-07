use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands;

#[derive(Parser)]
#[command(version, about, long_about = None)]
pub struct Cli {
    /// Optional name to operate on
    pub name: Option<String>,

    /// Sets a custom config file
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Turn debugging information on
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub debug: u8,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize HayCut in the current repository
    Init {
        /// Force initialization
        #[arg(short, long)]
        force: bool,
    },

    /// Trace token/context usage
    Trace,
}

pub fn run() {
    let cli = Cli::parse();
    cli.execute();
}

impl Cli {
    pub fn execute(self) {
        match self.command {
            Some(Commands::Init { force }) => {
                commands::init::run(force);
            }
            Some(Commands::Trace) => {
                commands::trace::run();
            }
            None => {}
        }
    }
}
