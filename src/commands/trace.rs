use std::{
    env, fs, io,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    cli::CompactorMode,
    compactor::{
        CompactPacket, CompactionInput, NativeHeuristicCompactor, OutputCompactor, RtkCompactor,
    },
    config::Config,
};

const ARTIFACT_ROOT: &str = ".haycut/runs";

#[derive(Debug)]
pub struct CommandTrace {
    pub command: String,
    pub args: Vec<String>,
    pub start_time: DateTime<Utc>,
    pub duration: Duration,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub working_directory: PathBuf,
}

#[derive(Debug)]
pub struct ArtifactPaths {
    pub run_directory: PathBuf,
    pub run_json: PathBuf,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub compact_json: PathBuf,
    pub compact_text: Option<PathBuf>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RunManifest {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub exit_code: i32,
    pub duration_ms: u128,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub estimated_raw_tokens: usize,
    pub raw_stdout_tokens_estimated: usize,
    pub raw_stderr_tokens_estimated: usize,
    pub created_at: DateTime<Utc>,
    pub stdout: String,
    pub stderr: String,
    pub compact: String,
}

impl RunManifest {
    pub fn load(path: &PathBuf) -> io::Result<Self> {
        let contents = fs::read_to_string(path)?;

        serde_json::from_str(&contents).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid run manifest {}: {error}", path.display()),
            )
        })
    }
}

pub fn run(command: Vec<String>, compactor: Option<CompactorMode>) -> i32 {
    if let Err(error) = Config::load_from_current_dir() {
        eprintln!("Error loading config: {error}");
        return 1;
    }

    match capture_command(command) {
        Ok(trace) => {
            match store_artifacts(&trace) {
                Ok(mut artifacts) => match compact_trace(&trace, &mut artifacts, compactor) {
                    Ok(packet) => print_trace(&trace, &artifacts, &packet),
                    Err(error) => {
                        eprintln!("Error compacting trace output: {error}");
                        return 1;
                    }
                },
                Err(error) => {
                    eprintln!("Error storing trace artifacts: {error}");
                    return 1;
                }
            }
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
    let start_time = Utc::now();
    let start = Instant::now();
    let output = Command::new(program).args(args).output()?;
    let duration = start.elapsed();

    Ok(CommandTrace {
        command: program.clone(),
        args: args.to_vec(),
        start_time,
        duration,
        exit_code: output.status.code().unwrap_or(1),
        stdout: output.stdout,
        stderr: output.stderr,
        working_directory,
    })
}

fn store_artifacts(trace: &CommandTrace) -> io::Result<ArtifactPaths> {
    store_artifacts_in(trace, &PathBuf::from(ARTIFACT_ROOT))
}

fn store_artifacts_in(trace: &CommandTrace, artifact_root: &PathBuf) -> io::Result<ArtifactPaths> {
    let id = run_id(trace.start_time);
    let run_directory = artifact_root.join(&id);
    let run_json = run_directory.join("run.json");
    let stdout = run_directory.join("stdout.txt");
    let stderr = run_directory.join("stderr.txt");
    let compact_json = run_directory.join("compact.json");

    fs::create_dir_all(&run_directory)?;
    fs::write(&stdout, &trace.stdout)?;
    fs::write(&stderr, &trace.stderr)?;
    let token_estimate = estimate_raw_tokens(trace);

    let manifest = RunManifest {
        id,
        command: command_line(trace),
        args: trace.args.clone(),
        cwd: trace.working_directory.display().to_string(),
        exit_code: trace.exit_code,
        duration_ms: trace.duration.as_millis(),
        stdout_bytes: trace.stdout.len(),
        stderr_bytes: trace.stderr.len(),
        estimated_raw_tokens: token_estimate.total,
        raw_stdout_tokens_estimated: token_estimate.stdout,
        raw_stderr_tokens_estimated: token_estimate.stderr,
        created_at: trace.start_time,
        stdout: "stdout.txt".to_string(),
        stderr: "stderr.txt".to_string(),
        compact: "compact.json".to_string(),
    };
    let metadata = serde_json::to_string_pretty(&manifest).map_err(io::Error::other)?;
    fs::write(&run_json, metadata)?;
    RunManifest::load(&run_json)?;

    Ok(ArtifactPaths {
        run_directory,
        run_json,
        stdout,
        stderr,
        compact_json,
        compact_text: None,
    })
}

fn compact_trace(
    trace: &CommandTrace,
    artifacts: &mut ArtifactPaths,
    mode: Option<CompactorMode>,
) -> io::Result<CompactPacket> {
    let input = CompactionInput {
        command: &trace.command,
        args: &trace.args,
        exit_code: trace.exit_code,
        duration: trace.duration,
        stdout: &trace.stdout,
        stderr: &trace.stderr,
        stdout_artifact: &artifacts.stdout,
        stderr_artifact: &artifacts.stderr,
    };

    let native = NativeHeuristicCompactor;
    let requested_mode = mode.unwrap_or_else(default_compactor_mode);
    let mut packet = match requested_mode {
        CompactorMode::Native => compact_with(&native, &input)?,
        CompactorMode::Rtk => match compact_with(&RtkCompactor, &input) {
            Ok(packet) => packet,
            Err(error) => {
                let mut packet = compact_with(&native, &input)?;
                packet.notes.push(format!(
                    "rtk unavailable or failed: {error}; fell back to native compaction"
                ));
                packet
            }
        },
    };

    store_compact_packet(&mut packet, artifacts)?;

    Ok(packet)
}

fn default_compactor_mode() -> CompactorMode {
    if RtkCompactor::is_installed() {
        CompactorMode::Rtk
    } else {
        CompactorMode::Native
    }
}

fn compact_with(
    compactor: &dyn OutputCompactor,
    input: &CompactionInput<'_>,
) -> io::Result<CompactPacket> {
    compactor.compact(input)
}

fn store_compact_packet(
    packet: &mut CompactPacket,
    artifacts: &mut ArtifactPaths,
) -> io::Result<()> {
    if let Some(compact_text) = packet.compact_text.as_deref() {
        let compact_text_path = artifacts.run_directory.join("compact.txt");
        fs::write(&compact_text_path, compact_text.as_bytes())?;
        packet.compact_artifact = Some(compact_text_path.display().to_string());
        artifacts.compact_text = Some(compact_text_path);
    }

    let json = serde_json::to_string_pretty(packet).map_err(io::Error::other)?;
    fs::write(&artifacts.compact_json, json)?;

    Ok(())
}

fn run_id(start_time: DateTime<Utc>) -> String {
    let timestamp = start_time.format("%Y-%m-%dT%H%M%SZ");
    let suffix = Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..6];

    format!("{timestamp}-{suffix}")
}

fn command_line(trace: &CommandTrace) -> String {
    if trace.args.is_empty() {
        return trace.command.clone();
    }

    format!("{} {}", trace.command, trace.args.join(" "))
}

#[derive(Debug, PartialEq, Eq)]
struct TokenEstimate {
    stdout: usize,
    stderr: usize,
    total: usize,
}

fn estimate_raw_tokens(trace: &CommandTrace) -> TokenEstimate {
    let stdout = estimate_tokens(&trace.stdout);
    let stderr = estimate_tokens(&trace.stderr);

    TokenEstimate {
        stdout,
        stderr,
        total: stdout + stderr,
    }
}

fn estimate_tokens(output: &[u8]) -> usize {
    String::from_utf8_lossy(output).chars().count() / 4
}

fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);

    for (index, digit) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(digit);
    }

    formatted.chars().rev().collect()
}

fn print_trace(trace: &CommandTrace, artifacts: &ArtifactPaths, packet: &CompactPacket) {
    let token_estimate = estimate_raw_tokens(trace);

    println!("command: {}", trace.command);
    println!("args: {:?}", trace.args);
    println!("start time: {}", trace.start_time.to_rfc3339());
    println!("duration: {:?}", trace.duration);
    println!("exit code: {}", trace.exit_code);
    println!("working directory: {}", trace.working_directory.display());
    println!("stdout bytes: {}", trace.stdout.len());
    println!("stderr bytes: {}", trace.stderr.len());
    println!(
        "Raw stdout tokens: {} estimated",
        format_count(token_estimate.stdout)
    );
    println!(
        "Raw stderr tokens: {} estimated",
        format_count(token_estimate.stderr)
    );
    println!(
        "Total raw tokens: {} estimated",
        format_count(token_estimate.total)
    );
    println!("Compactor: {}", packet.compactor);
    if let Some(version) = packet.rtk_version.as_deref() {
        println!("RTK version: {version}");
    }
    println!(
        "Packet tokens: {} estimated",
        format_count(packet.packet_tokens)
    );
    println!("Preserved items: {}", packet.preserved_items.len());
    println!("Omitted item groups: {}", packet.omitted_items.len());
    println!("artifact directory: {}", artifacts.run_directory.display());
    println!("run metadata: {}", artifacts.run_json.display());
    println!("compact packet: {}", artifacts.compact_json.display());
    println!("stdout artifact: {}", artifacts.stdout.display());
    println!("stderr artifact: {}", artifacts.stderr.display());
    if let Some(compact_text) = artifacts.compact_text.as_deref() {
        println!("compact artifact: {}", compact_text.display());
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

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
        assert_eq!(trace.stdout, b"hello");
        assert_eq!(trace.stderr, b"error");
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

    #[test]
    fn stores_raw_command_output_as_artifacts() {
        let trace = CommandTrace {
            command: "test-command".to_string(),
            args: vec!["--flag".to_string()],
            start_time: Utc::now(),
            duration: Duration::from_millis(42),
            exit_code: 3,
            stdout: vec![0, b'o', b'u', b't', 255],
            stderr: vec![b'e', b'r', b'r', 0],
            working_directory: env::current_dir().expect("current directory should exist"),
        };
        let artifact_root = env::temp_dir().join(format!(
            "haycut-artifacts-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos()
        ));

        let artifacts = store_artifacts_in(&trace, &artifact_root).expect("artifacts should store");

        assert!(artifacts.run_json.exists());
        assert_eq!(
            fs::read(&artifacts.stdout).expect("stdout should be readable"),
            trace.stdout
        );
        assert_eq!(
            fs::read(&artifacts.stderr).expect("stderr should be readable"),
            trace.stderr
        );

        let metadata =
            fs::read_to_string(&artifacts.run_json).expect("run metadata should be readable");
        assert!(metadata.contains("test-command"));
        assert!(metadata.contains("stdout.txt"));
        assert!(metadata.contains("stderr.txt"));
        assert!(metadata.contains("raw_stdout_tokens_estimated"));
        assert!(metadata.contains("raw_stderr_tokens_estimated"));
        assert!(metadata.contains("estimated_raw_tokens"));

        let manifest = RunManifest::load(&artifacts.run_json).expect("manifest should load");
        assert_eq!(
            manifest.id,
            artifacts
                .run_directory
                .file_name()
                .unwrap()
                .to_string_lossy()
        );
        assert_eq!(manifest.command, "test-command --flag");
        assert_eq!(manifest.args, vec!["--flag"]);
        assert_eq!(manifest.exit_code, 3);
        assert_eq!(manifest.stdout_bytes, trace.stdout.len());
        assert_eq!(manifest.stderr_bytes, trace.stderr.len());
        assert_eq!(manifest.estimated_raw_tokens, 2);

        fs::remove_dir_all(artifact_root).expect("test artifacts should be removed");
    }

    #[test]
    fn generated_run_ids_are_unique() {
        let start_time = Utc::now();
        let first = run_id(start_time);
        let second = run_id(start_time);

        assert_ne!(first, second);
        assert!(first.starts_with(&start_time.format("%Y-%m-%dT%H%M%SZ").to_string()));
    }

    #[test]
    fn corrupt_manifest_produces_clear_error() {
        let path = env::temp_dir().join(format!(
            "haycut-corrupt-manifest-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos()
        ));
        fs::write(&path, "not json").expect("corrupt manifest should be written");

        let error = RunManifest::load(&path).expect_err("corrupt manifest should fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("invalid run manifest"));
        assert!(error.to_string().contains(&path.display().to_string()));

        fs::remove_file(path).expect("corrupt manifest should be removed");
    }

    #[test]
    fn estimates_tokens_as_char_count_divided_by_four() {
        assert_eq!(estimate_tokens(b""), 0);
        assert_eq!(estimate_tokens(b"abcd"), 1);
        assert_eq!(estimate_tokens(b"abcdefg"), 1);
        assert_eq!(estimate_tokens("abcdé".as_bytes()), 1);
    }

    #[test]
    fn estimates_total_raw_tokens_for_stdout_and_stderr() {
        let trace = CommandTrace {
            command: "test-command".to_string(),
            args: Vec::new(),
            start_time: Utc::now(),
            duration: Duration::from_millis(1),
            exit_code: 0,
            stdout: b"abcdefgh".to_vec(),
            stderr: b"abcdefghijkl".to_vec(),
            working_directory: env::current_dir().expect("current directory should exist"),
        };

        assert_eq!(
            estimate_raw_tokens(&trace),
            TokenEstimate {
                stdout: 2,
                stderr: 3,
                total: 5,
            }
        );
    }

    #[test]
    fn formats_large_token_counts_with_commas() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(12), "12");
        assert_eq!(format_count(1234), "1,234");
        assert_eq!(format_count(1234567), "1,234,567");
    }
}
