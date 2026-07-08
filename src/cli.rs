use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::commands;

#[derive(Parser)]
#[command(version, about, long_about = None)]
pub struct Cli {
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

    /// Show indexed repository files by estimated token count
    Files {
        /// Maximum number of files to show
        #[arg(long, default_value_t = commands::files::DEFAULT_LIMIT)]
        limit: usize,
    },

    /// Walk the repository and count indexable files
    Index {
        /// Maximum file size to index, in bytes
        #[arg(long, default_value_t = commands::index::DEFAULT_MAX_FILE_SIZE_BYTES)]
        max_file_size: u64,
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

    /// List previous HayCut runs
    Runs {
        /// Maximum number of runs to show
        #[arg(long, default_value_t = commands::runs::DEFAULT_LIMIT)]
        limit: usize,
    },

    /// Show context reduction information for captured runs
    Report {
        /// Emit a machine-readable JSON report
        #[arg(long)]
        json: bool,

        /// Emit a Markdown report for issues, pull requests, and benchmark docs
        #[arg(long)]
        markdown: bool,

        /// Include a symbol snippet, e.g. src/main.rs::main
        #[arg(long = "symbol")]
        symbols: Vec<String>,
    },

    /// Build an evidence packet from a captured run
    Packet {
        /// Prune additional context to fit this token budget when possible
        #[arg(long)]
        budget: Option<usize>,

        /// Allow packets that exceed the hard token budget
        #[arg(long)]
        force: bool,
    },

    /// Read one parsed symbol by name or path::name
    ReadSymbol {
        /// Symbol name or path-qualified symbol, e.g. main or src/main.rs::main
        target: String,
    },

    /// Read a small line window from a file
    ReadWindow {
        /// File to read from
        path: PathBuf,

        /// Center line for the window
        #[arg(long)]
        line: usize,

        /// Number of lines to include before and after the center line
        #[arg(long, default_value_t = commands::read_window::DEFAULT_RADIUS)]
        radius: usize,

        /// Allow extremely large windows
        #[arg(long)]
        force: bool,
    },

    /// Suggest the next token-efficient debugging action without executing it
    Suggest,

    /// Search repository files for an exact string
    Search {
        /// Maximum number of matches to show
        #[arg(long, default_value_t = commands::search::DEFAULT_LIMIT)]
        limit: usize,

        /// Exact text to search for
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true)]
        query: Vec<String>,
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
            Some(Commands::Files { limit }) => commands::files::run(limit),
            Some(Commands::Index { max_file_size }) => commands::index::run(max_file_size),
            Some(Commands::Trace { compactor, command }) => {
                commands::trace::run(command, compactor)
            }
            Some(Commands::Runs { limit }) => commands::runs::run(limit),
            Some(Commands::Report {
                json,
                markdown,
                symbols,
            }) => commands::report::run(json, markdown, symbols),
            Some(Commands::Packet { budget, force }) => commands::packet::run(budget, force),
            Some(Commands::ReadSymbol { target }) => commands::read_symbol::run(target),
            Some(Commands::ReadWindow {
                path,
                line,
                radius,
                force,
            }) => commands::read_window::run(path, line, radius, force),
            Some(Commands::Suggest) => commands::suggest::run(),
            Some(Commands::Search { limit, query }) => commands::search::run(query, limit),
            None => 0,
        }
    }
}
