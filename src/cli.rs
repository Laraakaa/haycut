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
        /// Attach the trace to a task (`current` is supported)
        #[arg(long, value_enum)]
        task: Option<TaskTarget>,

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

    /// Show context reduction information for the latest captured run
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

    /// Manage HayCut task state
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },

    /// Run the constrained HayCut agent loop
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },

    /// Run gold-set eval cases against the agent
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },

    /// Search repository files for an exact string
    Search {
        /// Maximum number of matches to show
        #[arg(long, default_value_t = commands::search::DEFAULT_LIMIT)]
        limit: usize,

        /// Exact text to search for
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true)]
        query: Vec<String>,
    },

    /// Serve a local dashboard for analyzing eval and agent runs
    View {
        /// Port to serve the dashboard on
        #[arg(long, default_value_t = commands::view::DEFAULT_PORT)]
        port: u16,

        /// Directory containing eval results
        #[arg(long, default_value = "evals/results")]
        results_dir: PathBuf,
    },

    /// Launch the interactive terminal UI
    Tui {
        /// Run the scripted TUI demonstration instead of a real agent task
        #[arg(long)]
        demo: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CompactorMode {
    Native,
    Rtk,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum TaskTarget {
    Current,
}

#[derive(Subcommand)]
pub enum TaskCommand {
    /// Start a new current task
    Start {
        /// Task title and goal
        title: String,

        /// Verification command, e.g. `cargo test`
        #[arg(long)]
        verify: Option<String>,
    },

    /// Show current task status
    Status,

    /// List known tasks
    List,

    /// Close the current task
    Close,
}

#[derive(Subcommand)]
pub enum EvalCommand {
    /// List available eval cases
    List {
        /// Directory containing eval cases
        #[arg(long, default_value = "evals/cases")]
        cases_dir: PathBuf,
    },

    /// Run an eval case and write a report
    Run {
        /// Case name (directory under --cases-dir)
        case: String,

        /// Directory containing eval cases
        #[arg(long, default_value = "evals/cases")]
        cases_dir: PathBuf,

        /// Directory to write eval results into
        #[arg(long, default_value = "evals/results")]
        results_dir: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum AgentCommand {
    /// Loop until done, blocked, budget exhausted, or max steps hit
    Run {
        /// Use the current task
        #[arg(long, value_enum)]
        task: Option<TaskTarget>,

        /// Maximum planner steps to execute
        #[arg(long, default_value_t = commands::agent::DEFAULT_MAX_STEPS)]
        max_steps: usize,

        /// Apply planned edits. Without this flag, the agent previews changes only.
        #[arg(long, visible_alias = "yes")]
        apply: bool,

        /// Task goal when not using --task current
        #[arg(num_args = 0.., allow_hyphen_values = true)]
        goal: Vec<String>,
    },

    /// Ask the model for exactly one next action and execute it if deterministic
    Step {
        /// Use the current task
        #[arg(long, value_enum)]
        task: Option<TaskTarget>,
    },

    /// Show stored agent planner prompts, responses, actions, and cost
    Trace {
        /// Use the current task
        #[arg(long, value_enum)]
        task: Option<TaskTarget>,
    },

    /// Start an interactive terminal session driven by the engine control API
    Session {
        /// Use the current task
        #[arg(long, value_enum)]
        task: Option<TaskTarget>,

        /// Task goal when not using --task current
        #[arg(num_args = 0.., allow_hyphen_values = true)]
        goal: Vec<String>,
    },
}

pub fn run() -> i32 {
    let cli = Cli::parse();
    cli.execute()
}

impl Cli {
    pub fn execute(self) -> i32 {
        match self.command {
            Some(Commands::Init { force }) => commands::init::run(force),
            Some(Commands::Files { limit }) => commands::files::run(limit),
            Some(Commands::Index { max_file_size }) => commands::index::run(max_file_size),
            Some(Commands::Trace {
                task,
                compactor,
                command,
            }) => commands::trace::run(command, compactor, task),
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
            Some(Commands::Task { command }) => commands::task::run(command),
            Some(Commands::Agent { command }) => commands::agent::run(command),
            Some(Commands::Eval { command }) => match command {
                EvalCommand::List { cases_dir } => commands::eval::run_list(&cases_dir),
                EvalCommand::Run {
                    case,
                    cases_dir,
                    results_dir,
                } => commands::eval::run_case(&cases_dir, &results_dir, &case),
            },
            Some(Commands::Search { limit, query }) => commands::search::run(query, limit),
            Some(Commands::View { port, results_dir }) => commands::view::run(port, results_dir),
            Some(Commands::Tui { demo }) => commands::tui::run(demo),
            None => commands::tui::run(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_tui_command() {
        assert!(matches!(
            Cli::try_parse_from(["haycut", "tui"]).unwrap().command,
            Some(Commands::Tui { demo: false })
        ));
    }

    #[test]
    fn parses_tui_demo_mode() {
        assert!(matches!(
            Cli::try_parse_from(["haycut", "tui", "--demo"])
                .unwrap()
                .command,
            Some(Commands::Tui { demo: true })
        ));
    }

    #[test]
    fn bare_command_uses_default_route() {
        assert!(Cli::try_parse_from(["haycut"]).unwrap().command.is_none());
    }
}
