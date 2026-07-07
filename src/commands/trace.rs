use std::{
    env, io,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant, SystemTime},
};

use crate::config::Config;

#[derive(Debug)]
pub struct CommandTrace {
    pub command: String,
    pub args: Vec<String>,
    pub start_time: SystemTime,
    pub duration: Duration,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub working_directory: PathBuf,
}

pub fn run(command: Vec<String>) -> i32 {
    if let Err(error) = Config::load_from_current_dir() {
        eprintln!("Error loading config: {error}");
        return 1;
    }

    match capture_command(command) {
        Ok(trace) => {
            print_trace(&trace);
            trace.exit_code
        }
        Err(error) => {
            eprintln!("Error running command: {error}");
            1
        }
    }
}

fn capture_command(command: Vec<String>) -> io::Result<CommandTrace> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing command"))?;

    let working_directory = env::current_dir()?;
    let start_time = SystemTime::now();
    let start = Instant::now();
    let output = Command::new(program).args(args).output()?;
    let duration = start.elapsed();

    Ok(CommandTrace {
        command: program.clone(),
        args: args.to_vec(),
        start_time,
        duration,
        exit_code: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        working_directory,
    })
}

fn print_trace(trace: &CommandTrace) {
    println!("command: {}", trace.command);
    println!("args: {:?}", trace.args);
    println!("start time: {:?}", trace.start_time);
    println!("duration: {:?}", trace.duration);
    println!("exit code: {}", trace.exit_code);
    println!("working directory: {}", trace.working_directory.display());
    println!("stdout:\n{}", trace.stdout);
    eprintln!("stderr:\n{}", trace.stderr);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_successful_command_output() {
        let trace = capture_command(vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf hello && printf error >&2".to_string(),
        ])
        .expect("command should run");

        assert_eq!(trace.command, "sh");
        assert_eq!(trace.args, vec!["-c", "printf hello && printf error >&2"]);
        assert_eq!(trace.exit_code, 0);
        assert_eq!(trace.stdout, "hello");
        assert_eq!(trace.stderr, "error");
        assert!(trace.working_directory.is_absolute());
    }

    #[test]
    fn captures_failing_command_exit_code() {
        let trace = capture_command(vec![
            "sh".to_string(),
            "-c".to_string(),
            "exit 7".to_string(),
        ])
        .expect("command should run");

        assert_eq!(trace.exit_code, 7);
    }

    #[test]
    fn rejects_missing_command() {
        let error = capture_command(Vec::new()).expect_err("empty command should fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
