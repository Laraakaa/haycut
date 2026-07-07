use clap::{Parser, Subcommand, ValueEnum};
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
        /// Compactor to use for prompt-facing output
        #[arg(long, value_enum)]
        compactor: Option<CompactorMode>,

        /// Command and arguments to run
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
        command: Vec<String>,
    },

    /// Show context reduction information for captured runs
    Report {
        /// Report on the most recent run
        #[arg(long)]
        last: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CompactorMode {
    Native,
    Rtk,
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
            Some(Commands::Trace { compactor, command }) => {
                commands::trace::run(command, compactor)
            }
            Some(Commands::Report { last }) => commands::report::run(last),
            None => 0,
        }
    }
}
