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

    /// Run a command and capture trace information
    Trace {
        /// Command and arguments to run
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
        command: Vec<String>,
    },
}

pub fn run() -> i32 {
    let cli = Cli::parse();
    cli.execute()
}

impl Cli {
    pub fn execute(self) -> i32 {
        match self.command {
            Some(Commands::Init { force }) => {
                commands::init::run(force);
                0
            }
            Some(Commands::Trace { command }) => commands::trace::run(command),
            None => 0,
        }
    }
}
