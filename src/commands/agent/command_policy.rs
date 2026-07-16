//! Command risk classification, timeout, and cancellation for Phase 7 of
//! `plan_3_safety_and_execution.md`.
//!
//! Every command the agent wants to run is classified into a risk tier
//! before it ever touches a shell: `Low` runs are auto-allowed, `Medium`
//! runs require explicit user approval (mirroring the `--apply` gate patch
//! application already uses), and `High` runs are denied by default and
//! surfaced as a planner-visible observation instead of being executed.

use std::{
    io::{self, Read},
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// How risky a `(program, args)` pair is judged to be.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RiskTier {
    Low,
    Medium,
    High,
}

/// Default wall-clock timeout applied per risk tier. Higher-risk commands
/// (once approved) get less rope, since a hang is more costly to babysit.
pub fn timeout_for(tier: RiskTier) -> Duration {
    match tier {
        RiskTier::Low => Duration::from_secs(120),
        RiskTier::Medium => Duration::from_secs(90),
        RiskTier::High => Duration::from_secs(30),
    }
}

/// Classify a command by risk, per the table in `plan_3_safety_and_execution.md`:
/// Low = tests/build/lint/formatting/read-only Git, Medium = package
/// installation/codegen/migrations/commands that modify tracked files, High
/// = destructive filesystem or Git operations, network publishing, and
/// credential/secret access.
pub fn classify(program: &str, args: &[String]) -> RiskTier {
    let program_name = base_name(program);
    let joined_args = args.iter().map(|arg| arg.as_str()).collect::<Vec<_>>();

    if is_high_risk(&program_name, &joined_args) {
        return RiskTier::High;
    }
    if is_low_risk(&program_name, &joined_args) {
        return RiskTier::Low;
    }
    if is_medium_risk(&program_name, &joined_args) {
        return RiskTier::Medium;
    }

    // Unknown commands default to Medium: not automatically denied (that's
    // reserved for known-destructive patterns), but never silently run
    // without a human in the loop either.
    RiskTier::Medium
}

fn base_name(program: &str) -> String {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
        .to_string()
}

fn is_high_risk(program: &str, args: &[&str]) -> bool {
    let joined = args.join(" ");

    match program {
        "rm" => args
            .iter()
            .any(|arg| *arg == "-rf" || *arg == "-fr" || *arg == "-r" || *arg == "-f"),
        "git" => {
            let subcommand = args.first().copied().unwrap_or("");
            matches!(subcommand, "push") && args.iter().any(|arg| arg.contains("force"))
                || matches!(subcommand, "reset") && args.contains(&"--hard")
                || subcommand == "clean" && args.iter().any(|arg| arg.starts_with("-f"))
                || subcommand == "branch" && args.iter().any(|arg| *arg == "-D" || *arg == "-d")
        }
        "curl" | "wget" => {
            args.iter()
                .any(|arg| *arg == "-X" || *arg == "--upload-file")
                || joined.contains("--data")
                || joined.contains("POST")
                || joined.contains("PUT")
        }
        "cat" | "printenv" | "env" | "less" | "more" => {
            args.iter().any(|arg| looks_like_secret_path(arg))
        }
        "chmod" | "chown" => true,
        "dd" | "mkfs" | "shutdown" | "reboot" => true,
        "npm" | "pnpm" | "yarn" => args.first().copied() == Some("publish"),
        "cargo" => args.first().copied() == Some("publish"),
        "docker" => args.first().copied() == Some("push"),
        _ => false,
    }
}

fn looks_like_secret_path(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    lower.contains(".ssh")
        || lower.contains(".env")
        || lower.contains("credentials")
        || lower.contains("secret")
        || lower.contains(".aws")
        || lower.contains("id_rsa")
}

fn is_low_risk(program: &str, args: &[&str]) -> bool {
    match program {
        "cargo" => matches!(
            args.first().copied(),
            Some("test")
                | Some("build")
                | Some("check")
                | Some("fmt")
                | Some("clippy")
                | Some("doc")
        ),
        "go" => matches!(
            args.first().copied(),
            Some("test") | Some("build") | Some("vet") | Some("fmt")
        ),
        "npm" | "pnpm" | "yarn" | "bun" => {
            matches!(
                args.first().copied(),
                Some("test") | Some("run") | Some("build") | Some("lint")
            ) && !args.contains(&"publish")
        }
        "pytest" | "uv" | "poetry" | "pipenv" => true,
        "make" => true,
        "git" => {
            let subcommand = args.first().copied().unwrap_or("");
            matches!(
                subcommand,
                "status" | "log" | "diff" | "show" | "branch" | "rev-parse" | "blame" | "fetch"
            ) && !args.iter().any(|arg| *arg == "-D" || *arg == "-d")
        }
        _ => false,
    }
}

fn is_medium_risk(program: &str, args: &[&str]) -> bool {
    let subcommand = args.first().copied().unwrap_or("");
    match program {
        "npm" | "pnpm" | "yarn" | "bun" => {
            matches!(subcommand, "install" | "add" | "remove" | "update")
        }
        "pip" | "pip3" => matches!(subcommand, "install" | "uninstall"),
        "cargo" => matches!(subcommand, "add" | "remove" | "update" | "install"),
        "git" => matches!(
            subcommand,
            "commit"
                | "add"
                | "checkout"
                | "merge"
                | "rebase"
                | "push"
                | "reset"
                | "restore"
                | "tag"
                | "stash"
        ),
        "alembic" | "flyway" | "sqlx" => true,
        _ => false,
    }
}

/// The outcome of running a command under a timeout.
#[derive(Debug)]
pub struct CommandOutcome {
    /// `None` when the process was killed for exceeding its timeout.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration: Duration,
    pub stdout: String,
    pub stderr: String,
}

/// Run `program args` in `cwd`, killing it if it runs longer than `timeout`.
/// Stdout/stderr are drained concurrently on background threads so a chatty
/// process can't deadlock on a full pipe while we poll for completion.
pub fn run_with_timeout(
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
) -> io::Result<CommandOutcome> {
    let start = Instant::now();
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn()?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_handle = std::thread::spawn(move || drain(stdout_pipe));
    let stderr_handle = std::thread::spawn(move || drain(stderr_pipe));

    let mut timed_out = false;
    let exit_status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                if start.elapsed() >= timeout {
                    #[cfg(unix)]
                    // The command may have spawned descendants that still own
                    // the output pipes. Kill the whole group before joining
                    // the drain threads so timeout cleanup cannot wait for
                    // those descendants to finish naturally.
                    unsafe {
                        let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        }
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();

    Ok(CommandOutcome {
        exit_code: exit_status.and_then(|status| status.code()),
        timed_out,
        duration: start.elapsed(),
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
    })
}

fn drain(pipe: Option<impl Read>) -> Vec<u8> {
    let mut buffer = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut buffer);
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn low_risk_commands() {
        assert_eq!(classify("cargo", &args(&["test"])), RiskTier::Low);
        assert_eq!(classify("cargo", &args(&["build"])), RiskTier::Low);
        assert_eq!(classify("cargo", &args(&["fmt", "--check"])), RiskTier::Low);
        assert_eq!(
            classify("git", &args(&["status", "--porcelain"])),
            RiskTier::Low
        );
        assert_eq!(classify("git", &args(&["log", "-1"])), RiskTier::Low);
        assert_eq!(classify("git", &args(&["diff"])), RiskTier::Low);
    }

    #[test]
    fn medium_risk_commands() {
        assert_eq!(classify("npm", &args(&["install"])), RiskTier::Medium);
        assert_eq!(
            classify("pip", &args(&["install", "requests"])),
            RiskTier::Medium
        );
        assert_eq!(
            classify("cargo", &args(&["add", "serde"])),
            RiskTier::Medium
        );
        assert_eq!(
            classify("git", &args(&["commit", "-m", "x"])),
            RiskTier::Medium
        );
        assert_eq!(
            classify("alembic", &args(&["upgrade", "head"])),
            RiskTier::Medium
        );
    }

    #[test]
    fn high_risk_commands() {
        assert_eq!(classify("rm", &args(&["-rf", "/"])), RiskTier::High);
        assert_eq!(classify("git", &args(&["push", "--force"])), RiskTier::High);
        assert_eq!(classify("git", &args(&["reset", "--hard"])), RiskTier::High);
        assert_eq!(classify("cat", &args(&["~/.ssh/id_rsa"])), RiskTier::High);
        assert_eq!(classify("cargo", &args(&["publish"])), RiskTier::High);
        assert_eq!(
            classify("chmod", &args(&["777", "/etc/passwd"])),
            RiskTier::High
        );
    }

    #[test]
    fn unknown_commands_default_to_medium_not_auto_run() {
        assert_eq!(
            classify("some-custom-tool", &args(&["--do-thing"])),
            RiskTier::Medium
        );
    }

    #[test]
    fn run_with_timeout_captures_exit_and_output() {
        let outcome = run_with_timeout(
            "sh",
            &args(&["-c", "echo hello; echo world 1>&2; exit 3"]),
            Path::new("."),
            Duration::from_secs(5),
        )
        .expect("command should run");

        assert_eq!(outcome.exit_code, Some(3));
        assert!(!outcome.timed_out);
        assert!(outcome.stdout.contains("hello"));
        assert!(outcome.stderr.contains("world"));
    }

    #[test]
    fn run_with_timeout_kills_long_running_commands() {
        let outcome = run_with_timeout(
            "sh",
            &args(&["-c", "sleep 5"]),
            Path::new("."),
            Duration::from_millis(200),
        )
        .expect("command should be killable");

        assert!(outcome.timed_out);
        assert!(outcome.exit_code.is_none());
        assert!(outcome.duration < Duration::from_secs(2));
    }
}
