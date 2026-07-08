use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    commands::read_window::DEFAULT_RADIUS,
    commands::run_context::RunContext,
    commands::trace::RunManifest,
    compactor::{CompactPacket, PreservedKind},
    extract::extract_file_references,
    store::RUN_STORE_PATH,
    util::estimate_tokens,
};

pub fn run() -> i32 {
    match load_suggestions(Path::new(RUN_STORE_PATH)) {
        Ok(suggestions) => {
            print_suggestions(&suggestions);
            0
        }
        Err(error) => {
            eprintln!("Error loading run: {error}");
            1
        }
    }
}

#[derive(Debug)]
pub struct Suggestion {
    pub action: String,
    pub reason: String,
    pub estimated_tokens: usize,
}

fn load_suggestions(db_path: &Path) -> io::Result<Vec<Suggestion>> {
    let ctx = RunContext::load_last(db_path)?;
    // The compacted packet can truncate long lines (e.g. RTK collapses a panic
    // location to "…/runtime…"), which destroys the file:line data we need. The
    // raw captured output is stored on disk untruncated, so read it directly to
    // recover accurate source locations. Reading it is local-only work; nothing
    // here is forwarded to a model.
    let raw_stdout = ctx.read_stdout_lossy().unwrap_or_default();
    let raw_stderr = ctx.read_stderr_lossy().unwrap_or_default();
    Ok(derive_suggestions(
        &ctx.manifest,
        &ctx.compact,
        &raw_stdout,
        &raw_stderr,
    ))
}

const MAX_SUGGESTIONS: usize = 3;

fn derive_suggestions(
    manifest: &RunManifest,
    packet: &CompactPacket,
    raw_stdout: &str,
    raw_stderr: &str,
) -> Vec<Suggestion> {
    let cwd = Path::new(&manifest.cwd);

    // Prefer the raw, untruncated output as the source of truth. stderr usually
    // carries the primary diagnostic, so scan it first, then stdout.
    // extract_file_references deduplicates by (path, line) within each call;
    // we deduplicate across both streams below.
    let mut file_refs = extract_file_references(raw_stderr, "stderr");
    for r in extract_file_references(raw_stdout, "stdout") {
        if !file_refs
            .iter()
            .any(|e| e.path == r.path && e.line == r.line)
        {
            file_refs.push(r);
        }
    }

    // Fall back to the compacted preserved items for stack-trace summaries
    // produced by the native compactor ("Likely stack trace: - path:line").
    if file_refs.is_empty() {
        for item in &packet.preserved_items {
            if item.kind == PreservedKind::StackTrace {
                for r in extract_file_references(&item.line, "compact") {
                    if !file_refs
                        .iter()
                        .any(|e| e.path == r.path && e.line == r.line)
                    {
                        file_refs.push(r);
                    }
                }
            } else {
                for r in extract_file_references(&item.line, "compact") {
                    if !file_refs
                        .iter()
                        .any(|e| e.path == r.path && e.line == r.line)
                    {
                        file_refs.push(r);
                    }
                }
            }
        }
    }

    let mut suggestions = Vec::new();
    for file_ref in file_refs
        .into_iter()
        .filter(|r| is_project_local(&r.path, cwd))
        .take(MAX_SUGGESTIONS)
    {
        let abs_path = if file_ref.path.starts_with('/') {
            PathBuf::from(&file_ref.path)
        } else {
            cwd.join(&file_ref.path)
        };

        let token_estimate = estimate_window_tokens(&abs_path, file_ref.line, DEFAULT_RADIUS);

        suggestions.push(Suggestion {
            action: format!(
                "read-window {} --line {} --radius {}",
                file_ref.path, file_ref.line, DEFAULT_RADIUS
            ),
            reason: format!(
                "output points to {}:{}  this is cheaper than reading the full file",
                file_ref.path, file_ref.line
            ),
            estimated_tokens: token_estimate,
        });
    }

    suggestions
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Vendor / toolchain path fragments whose locations are rarely worth reading
/// during debugging. Suggesting a window into a dependency's source is noise.
const VENDOR_MARKERS: [&str; 6] = [
    ".cargo/registry",
    ".rustup/",
    "/rustc/",
    "node_modules/",
    "site-packages/",
    "/dist-packages/",
];

/// Returns true when the reference points at code the user is likely to own:
/// a relative path, or an absolute path under the run's working directory, and
/// never a known dependency/toolchain location.
fn is_project_local(path: &str, cwd: &Path) -> bool {
    if VENDOR_MARKERS.iter().any(|marker| path.contains(marker)) {
        return false;
    }

    if path.starts_with('/') {
        return Path::new(path).starts_with(cwd);
    }

    // Reject paths that try to escape the project root.
    !path.starts_with("../")
}

fn estimate_window_tokens(path: &Path, center_line: usize, radius: usize) -> usize {
    let Ok(contents) = fs::read_to_string(path) else {
        // Fallback: rough estimate assuming 80 chars/line, 4 chars/token
        return (radius * 2 + 1) * 20;
    };

    let lines: Vec<&str> = contents.lines().collect();
    let start = center_line.saturating_sub(radius + 1);
    let end = (center_line + radius).min(lines.len());
    let window_text = lines[start..end].join("\n");
    estimate_tokens(window_text.as_bytes())
}

fn print_suggestions(suggestions: &[Suggestion]) {
    if suggestions.is_empty() {
        println!("No token-efficient next action found for the last run.");
        println!("Run `haycut report --last` to inspect the preserved evidence.");
        return;
    }

    for (i, suggestion) in suggestions.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("Suggested next action:  {}", suggestion.action);
        println!("Reason:                 {}", suggestion.reason);
        println!(
            "Estimated cost:         {} tokens",
            suggestion.estimated_tokens
        );
    }
}
