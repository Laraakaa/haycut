use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    budget::BudgetUsage,
    commands::trace::RunManifest,
    compactor::{CompactPacket, OutputSource, PreservedItem, PreservedKind, estimate_tokens},
    config::{Config, TokenConfig},
    store::{self, RUN_STORE_PATH},
};

const EXCERPT_RADIUS: usize = 2;

pub fn run(last: bool, budget: Option<usize>, force: bool) -> i32 {
    if !last {
        eprintln!("Error: packet currently requires --last");
        return 2;
    }

    let config = match Config::load_from_current_dir() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Error loading config: {error}");
            return 1;
        }
    };

    match load_last_failed_packet(Path::new(RUN_STORE_PATH)) {
        Ok(mut packet) => {
            if let Some(budget) = budget {
                packet.prune_to_budget(budget);
            }

            let budget = packet.budget_usage(&config.token);
            if let Some(error) = budget.hard_error().filter(|_| !force) {
                eprint!("{}", budget.render());
                eprintln!("Error: {error}");
                return 2;
            }

            print!("{}", packet.render(&config.token));
            0
        }
        Err(error) => {
            eprintln!("Error loading packet: {error}");
            1
        }
    }
}

#[derive(Debug)]
pub struct EvidencePacket {
    pub title: String,
    pub summary: Vec<String>,
    pub items: Vec<ContextItem>,
    pub omitted: Vec<OmittedItem>,
    pub raw_token_estimate: usize,
    pub base_token_estimate: usize,
    pub token_estimate: usize,
    pub full_handles: Vec<ArtifactHandle>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ContextItem {
    pub kind: ContextKind,
    pub content: String,
    pub source: SourceRef,
    pub reason: String,
    pub priority: Priority,
    pub token_estimate: usize,
}

#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub enum ContextKind {
    CommandSummary,
    FailureLine,
    StackFrame,
    Assertion,
    FileReference,
    CodeWindow,
    Symbol,
    Diff,
    Note,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ArtifactHandle {
    pub kind: ArtifactKind,
    pub path: PathBuf,
    pub token_estimate: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactKind {
    Stdout,
    Stderr,
    Compact,
}

#[derive(Debug, PartialEq, Eq)]
pub struct OmittedItem {
    pub source: SourceRef,
    pub reason: String,
    pub count: usize,
    pub token_estimate: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SourceRef {
    Run {
        id: String,
    },
    Output {
        kind: ArtifactKind,
    },
    File {
        path: String,
        line: usize,
    },
    CodeWindow {
        path: String,
        start_line: usize,
        end_line: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    High,
    Medium,
    Low,
}

#[derive(Debug, PartialEq, Eq)]
struct FileMention {
    path: String,
    line: usize,
    excerpt: Option<LineExcerpt>,
}

#[derive(Debug, PartialEq, Eq)]
struct LineExcerpt {
    start_line: usize,
    lines: Vec<String>,
}

impl EvidencePacket {
    fn render(&self, token_config: &TokenConfig) -> String {
        let mut output = String::new();

        output.push_str(&self.title);
        output.push('\n');
        for line in &self.summary {
            output.push_str(line);
            output.push('\n');
        }
        output.push_str("Included context:\n");
        for item in &self.items {
            output.push_str(&format!(
                "  - source: {}  tokens: {}  reason: {}\n",
                item.source.label(),
                format_count(item.token_estimate),
                item.reason
            ));
        }
        output.push_str("Files mentioned:\n");
        let file_references = self.file_references();
        if file_references.is_empty() {
            output.push_str("  none detected\n");
        } else {
            for item in file_references {
                let SourceRef::File { path, line } = &item.source else {
                    continue;
                };
                output.push_str(&format!("  {path}:{line}\n"));
                match self.code_window_for(path, *line) {
                    Some(window) => {
                        let SourceRef::CodeWindow {
                            start_line,
                            end_line,
                            ..
                        } = window.source
                        else {
                            continue;
                        };
                        output
                            .push_str(&format!("  <excerpt lines {}-{}>\n", start_line, end_line));
                        for (offset, line) in window.content.lines().enumerate() {
                            output.push_str(&format!(
                                "    {:>4} | {}\n",
                                start_line + offset,
                                line
                            ));
                        }
                        output.push_str("  </excerpt>\n");
                    }
                    None => output.push_str("    excerpt unavailable\n"),
                }
            }
        }
        output.push_str("Context budget:\n");
        output.push_str(&format!(
            "  packet tokens: {}\n",
            format_count(self.token_estimate)
        ));
        output.push_str(&self.budget_usage(token_config).render());
        if !self.omitted.is_empty() {
            output.push_str("Omitted:\n");
            for item in &self.omitted {
                output.push_str(&format!(
                    "  - source: {}  tokens: {}  reason: {}\n",
                    item.source.label(),
                    format_count(item.token_estimate),
                    item.reason
                ));
            }
        }
        output.push_str("Full handles:\n");
        for handle in &self.full_handles {
            output.push_str(&format!(
                "  {}: {}\n",
                handle.kind.label(),
                handle.path.display()
            ));
        }

        output
    }

    fn budget_usage(&self, token_config: &TokenConfig) -> BudgetUsage {
        BudgetUsage::from_config(token_config, self.raw_token_estimate, self.token_estimate)
    }

    fn prune_to_budget(&mut self, budget: usize) {
        let mut fixed_items = Vec::new();
        let mut prunable_items = Vec::new();
        for (index, item) in self.items.drain(..).enumerate() {
            if item.is_prunable_context() {
                prunable_items.push((index, item));
            } else {
                fixed_items.push((index, item));
            }
        }

        prunable_items.sort_by_key(|(index, item)| (item.priority.prune_rank(), *index));
        let mut token_estimate = self.base_token_estimate;
        let mut kept_items = fixed_items;

        for (index, item) in prunable_items {
            if token_estimate + item.token_estimate <= budget {
                token_estimate += item.token_estimate;
                kept_items.push((index, item));
            } else {
                self.omitted.push(OmittedItem {
                    source: item.source,
                    reason: format!("over budget; {}", item.reason),
                    count: 0,
                    token_estimate: item.token_estimate,
                });
            }
        }

        kept_items.sort_by_key(|(index, item)| (item.priority.render_rank(), *index));
        self.items = kept_items.into_iter().map(|(_, item)| item).collect();
        self.token_estimate = token_estimate;
    }

    fn file_references(&self) -> Vec<&ContextItem> {
        self.items
            .iter()
            .filter(|item| item.kind == ContextKind::FileReference)
            .collect()
    }

    fn code_window_for(&self, path: &str, line: usize) -> Option<&ContextItem> {
        self.items.iter().find(|item| match &item.source {
            SourceRef::CodeWindow {
                path: window_path,
                start_line,
                end_line,
            } => {
                item.kind == ContextKind::CodeWindow
                    && window_path == path
                    && (*start_line..=*end_line).contains(&line)
            }
            _ => false,
        })
    }
}

impl ContextItem {
    fn is_prunable_context(&self) -> bool {
        matches!(
            self.kind,
            ContextKind::FileReference | ContextKind::CodeWindow
        )
    }
}

impl Priority {
    fn prune_rank(self) -> u8 {
        match self {
            Priority::High => 0,
            Priority::Medium => 1,
            Priority::Low => 2,
        }
    }

    fn render_rank(self) -> u8 {
        self.prune_rank()
    }
}

impl ArtifactKind {
    fn label(self) -> &'static str {
        match self {
            ArtifactKind::Stdout => "stdout",
            ArtifactKind::Stderr => "stderr",
            ArtifactKind::Compact => "compact",
        }
    }
}

impl SourceRef {
    fn label(&self) -> String {
        match self {
            SourceRef::Run { id } => format!("run {id}"),
            SourceRef::Output { kind } => kind.label().to_string(),
            SourceRef::File { path, line } => format!("{path}:{line}"),
            SourceRef::CodeWindow {
                path,
                start_line,
                end_line,
            } => format!("{path} lines {start_line}-{end_line}"),
        }
    }
}

fn load_last_failed_packet(db_path: &Path) -> io::Result<EvidencePacket> {
    let stored_run = store::latest_failed_run(db_path)?;
    let run_json_path = PathBuf::from(stored_run.artifact_path("run_manifest")?);
    let compact_path = PathBuf::from(stored_run.artifact_path("compact_json")?);
    let run_directory = run_json_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "run manifest has no parent directory",
            )
        })?;
    let manifest = RunManifest::load(&run_json_path)?;
    let compact_contents = fs::read_to_string(&compact_path)?;
    let compact: CompactPacket = serde_json::from_str(&compact_contents).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid compact packet {}: {error}", compact_path.display()),
        )
    })?;
    let stdout = read_lossy(&run_directory.join(&manifest.stdout))?;
    let stderr = read_lossy(&run_directory.join(&manifest.stderr))?;
    let mentions = file_mentions(
        Path::new(&manifest.cwd),
        &[stdout, stderr, compact_lines(&compact)],
    );

    Ok(build_evidence_packet(
        run_directory,
        manifest,
        compact,
        mentions,
    ))
}

fn build_evidence_packet(
    run_directory: PathBuf,
    manifest: RunManifest,
    compact: CompactPacket,
    mentions: Vec<FileMention>,
) -> EvidencePacket {
    let mut items = vec![command_summary_item(&manifest)];
    items.extend(
        compact
            .preserved_items
            .iter()
            .map(context_item_from_preserved),
    );
    items.extend(mentions.iter().flat_map(context_items_from_mention));

    let likely_failure = likely_failure_from_items(&items).unwrap_or("none detected");
    let summary = vec![
        format!("Run:      {}", manifest.id),
        format!(
            "Command:  {}  exit code: {}",
            manifest.command, manifest.exit_code
        ),
        format!("Likely failure:  {likely_failure}"),
    ];
    let mention_tokens = items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                ContextKind::FileReference | ContextKind::CodeWindow
            )
        })
        .map(|item| item.token_estimate)
        .sum::<usize>();
    let token_estimate = compact.packet_tokens + mention_tokens;
    let base_token_estimate = compact.packet_tokens;
    let full_handles = artifact_handles(&run_directory, &manifest, &compact);
    let omitted = compact
        .omitted_items
        .iter()
        .map(|item| OmittedItem {
            source: source_ref_for_output(item.source),
            reason: item.reason.clone(),
            count: item.count,
            token_estimate: omitted_token_estimate(&compact, item.source),
        })
        .collect();

    EvidencePacket {
        title: "EVIDENCE PACKET".to_string(),
        summary,
        items,
        omitted,
        raw_token_estimate: compact.raw_tokens,
        base_token_estimate,
        token_estimate,
        full_handles,
    }
}

fn omitted_token_estimate(compact: &CompactPacket, source: OutputSource) -> usize {
    match source {
        OutputSource::Stdout => compact.raw_stdout_tokens,
        OutputSource::Stderr => compact.raw_stderr_tokens,
        OutputSource::Rtk => compact.packet_tokens,
    }
}

fn command_summary_item(manifest: &RunManifest) -> ContextItem {
    let content = format!(
        "{} exited with {} after {}ms",
        manifest.command, manifest.exit_code, manifest.duration_ms
    );

    ContextItem {
        kind: ContextKind::CommandSummary,
        token_estimate: estimate_tokens(content.as_bytes()),
        content,
        source: SourceRef::Run {
            id: manifest.id.clone(),
        },
        reason: "summarizes the captured command run".to_string(),
        priority: Priority::High,
    }
}

fn context_item_from_preserved(item: &PreservedItem) -> ContextItem {
    let kind = match item.kind {
        PreservedKind::ErrorLine => ContextKind::FailureLine,
        PreservedKind::StackTrace => ContextKind::StackFrame,
        PreservedKind::RtkFiltered => ContextKind::Note,
    };
    let priority = match kind {
        ContextKind::FailureLine | ContextKind::StackFrame => Priority::High,
        ContextKind::Note => Priority::Low,
        _ => Priority::Medium,
    };

    ContextItem {
        kind,
        content: item.line.clone(),
        source: source_ref_for_output(item.source),
        reason: preserved_reason(item.kind).to_string(),
        priority,
        token_estimate: estimate_tokens(item.line.as_bytes()),
    }
}

fn context_items_from_mention(mention: &FileMention) -> Vec<ContextItem> {
    let reference_content = format!("{}:{}", mention.path, mention.line);
    let mut items = vec![ContextItem {
        kind: ContextKind::FileReference,
        token_estimate: estimate_tokens(reference_content.as_bytes()),
        content: reference_content,
        source: SourceRef::File {
            path: mention.path.clone(),
            line: mention.line,
        },
        reason: "file location mentioned in captured output".to_string(),
        priority: Priority::Medium,
    }];

    if let Some(excerpt) = &mention.excerpt {
        let content = excerpt.lines.join("\n");
        let end_line = excerpt.start_line + excerpt.lines.len().saturating_sub(1);
        items.push(ContextItem {
            kind: ContextKind::CodeWindow,
            token_estimate: estimate_tokens(content.as_bytes()),
            content,
            source: SourceRef::CodeWindow {
                path: mention.path.clone(),
                start_line: excerpt.start_line,
                end_line,
            },
            reason: "nearby source context for a referenced location".to_string(),
            priority: Priority::Medium,
        });
    }

    items
}

fn artifact_handles(
    run_directory: &Path,
    manifest: &RunManifest,
    compact: &CompactPacket,
) -> Vec<ArtifactHandle> {
    vec![
        ArtifactHandle {
            kind: ArtifactKind::Stdout,
            path: run_directory.join(&manifest.stdout),
            token_estimate: compact.raw_stdout_tokens,
        },
        ArtifactHandle {
            kind: ArtifactKind::Stderr,
            path: run_directory.join(&manifest.stderr),
            token_estimate: compact.raw_stderr_tokens,
        },
        ArtifactHandle {
            kind: ArtifactKind::Compact,
            path: run_directory.join(&manifest.compact),
            token_estimate: compact.packet_tokens,
        },
    ]
}

fn source_ref_for_output(source: OutputSource) -> SourceRef {
    let kind = match source {
        OutputSource::Stdout => ArtifactKind::Stdout,
        OutputSource::Stderr => ArtifactKind::Stderr,
        OutputSource::Rtk => ArtifactKind::Compact,
    };

    SourceRef::Output { kind }
}

fn preserved_reason(kind: PreservedKind) -> &'static str {
    match kind {
        PreservedKind::ErrorLine => "failure line preserved from command output",
        PreservedKind::StackTrace => "stack frame summary preserved from command output",
        PreservedKind::RtkFiltered => "line preserved by RTK compaction",
    }
}

fn likely_failure_from_items(items: &[ContextItem]) -> Option<&str> {
    items
        .iter()
        .find(|item| item.kind == ContextKind::FailureLine && !item.content.trim().is_empty())
        .map(|item| item.content.as_str())
}

fn file_mentions(root: &Path, texts: &[String]) -> Vec<FileMention> {
    let mut locations = BTreeSet::new();
    for text in texts {
        for (path, line) in extract_locations(text) {
            locations.insert((path, line));
        }
    }

    locations
        .into_iter()
        .map(|(path, line)| {
            let excerpt = source_excerpt(root, &path, line).ok().flatten();
            FileMention {
                path,
                line,
                excerpt,
            }
        })
        .collect()
}

fn extract_locations(text: &str) -> Vec<(String, usize)> {
    let mut locations = Vec::new();

    for line in text.lines() {
        if let Some(location) = parse_python_traceback_location(line) {
            locations.push(location);
        }

        locations.extend(parse_colon_locations(line));
    }

    locations
}

#[cfg(test)]
fn parse_location(token: &str) -> Option<(String, usize)> {
    let token = token.trim_matches(|ch: char| {
        matches!(
            ch,
            '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '\'' | '"'
        )
    });

    parse_location_prefix(token)
}

fn parse_colon_locations(line: &str) -> Vec<(String, usize)> {
    let mut locations = Vec::new();

    for (start, ch) in line.char_indices() {
        if !is_path_start(ch) || !is_location_start_boundary(line, start) {
            continue;
        }

        if let Some(location) = parse_location_prefix(&line[start..]) {
            locations.push(location);
        }
    }

    locations
}

fn parse_location_prefix(text: &str) -> Option<(String, usize)> {
    for (index, ch) in text.char_indices() {
        if ch != ':' {
            continue;
        }

        let path = &text[..index];
        let line_number = text[index + ch.len_utf8()..]
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        let Ok(line) = line_number.parse::<usize>() else {
            continue;
        };
        if line == 0 || !looks_like_source_path(path) {
            continue;
        }

        return Some((path.trim_start_matches("./").to_string(), line));
    }

    None
}

fn is_path_start(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '.' || ch == '/'
}

fn is_location_start_boundary(line: &str, start: usize) -> bool {
    if start == 0 {
        return true;
    }

    let before = &line[..start];
    let Some(previous) = before.chars().next_back() else {
        return true;
    };
    if !is_path_char(previous) {
        return true;
    }

    previous.is_ascii_digit() && before_number_is_colon(before)
}

fn before_number_is_colon(text: &str) -> bool {
    text.trim_end_matches(|ch: char| ch.is_ascii_digit())
        .ends_with(':')
}

fn is_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/' | ':' | '\\')
}

fn parse_python_traceback_location(line: &str) -> Option<(String, usize)> {
    let file_start = line.find("File ")?;
    let after_file = &line[file_start + "File ".len()..];
    let quote = after_file.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }

    let after_open_quote = &after_file[quote.len_utf8()..];
    let path_end = after_open_quote.find(quote)?;
    let path = &after_open_quote[..path_end];
    let after_path = &after_open_quote[path_end + quote.len_utf8()..];
    let line_marker = ", line ";
    let line_start = after_path.find(line_marker)? + line_marker.len();
    let line_number = after_path[line_start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    let line = line_number.parse::<usize>().ok()?;
    if line == 0 || !looks_like_source_path(path) {
        return None;
    }

    Some((path.trim_start_matches("./").to_string(), line))
}

fn looks_like_source_path(path: &str) -> bool {
    if path.is_empty() || !path.chars().all(is_path_char) {
        return false;
    }

    if path.starts_with("http://") || path.starts_with("https://") {
        return false;
    }

    path.contains('/')
        || path.ends_with(".rs")
        || path.ends_with(".py")
        || path.ends_with(".ts")
        || path.ends_with(".tsx")
        || path.ends_with(".js")
        || path.ends_with(".jsx")
}

fn source_excerpt(root: &Path, path: &str, line: usize) -> io::Result<Option<LineExcerpt>> {
    let path = Path::new(path);
    let source_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let source = match fs::read_to_string(source_path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let lines: Vec<&str> = source.lines().collect();
    if line == 0 || line > lines.len() {
        return Ok(None);
    }

    let start_line = line.saturating_sub(EXCERPT_RADIUS).max(1);
    let end_line = (line + EXCERPT_RADIUS).min(lines.len());
    let excerpt = lines[start_line - 1..end_line]
        .iter()
        .map(|line| (*line).to_string())
        .collect();

    Ok(Some(LineExcerpt {
        start_line,
        lines: excerpt,
    }))
}

fn compact_lines(compact: &CompactPacket) -> String {
    compact
        .preserved_items
        .iter()
        .map(|item| item.line.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_lossy(path: &Path) -> io::Result<String> {
    fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
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

#[cfg(test)]
mod tests {
    use std::{
        env,
        time::{SystemTime, UNIX_EPOCH},
    };

    use chrono::Utc;

    use super::*;
    use crate::{
        compactor::{OutputSource, PreservedItem},
        store::{NewArtifact, NewRun, insert_run},
    };

    #[test]
    fn parses_file_locations_from_output_tokens() {
        assert_eq!(
            parse_location("src/lib.rs:12:5"),
            Some(("src/lib.rs".to_string(), 12))
        );
        assert_eq!(
            parse_location("tests/auth/session_test.rs:52:9"),
            Some(("tests/auth/session_test.rs".to_string(), 52))
        );
        assert_eq!(
            parse_location("tests/test_auth.py:52"),
            Some(("tests/test_auth.py".to_string(), 52))
        );
        assert_eq!(
            parse_location("/path/to/file.ts:117:13"),
            Some(("/path/to/file.ts".to_string(), 117))
        );
        assert_eq!(
            parse_location("(./src/auth/session.rs:88)"),
            Some(("src/auth/session.rs".to_string(), 88))
        );
        assert_eq!(parse_location("exit code: 101"), None);
    }

    #[test]
    fn extracts_python_traceback_file_locations() {
        assert_eq!(
            parse_python_traceback_location(
                "  File \"tests/test_auth.py\", line 123, in test_auth"
            ),
            Some(("tests/test_auth.py".to_string(), 123))
        );
        assert_eq!(
            parse_python_traceback_location("File '/path/to/file.ts', line 117"),
            Some(("/path/to/file.ts".to_string(), 117))
        );
        assert_eq!(
            parse_python_traceback_location("File missing quotes, line 123"),
            None
        );
    }

    #[test]
    fn extracts_locations_from_multiline_logs() {
        let locations = extract_locations(
            "error[E0425]: cannot find value\n  --> src/lib.rs:12:5\n  File \"tests/test_auth.py\", line 52, in test_auth\n",
        );

        assert!(locations.contains(&("src/lib.rs".to_string(), 12)));
        assert!(locations.contains(&("tests/test_auth.py".to_string(), 52)));
    }

    #[test]
    fn extracts_adjacent_path_locations_from_logs() {
        let locations = extract_locations(
            "src/lib.rs:12:5tests/test_auth.py:52/path/to/file.ts:117:13File \"src/main.rs\", line 9",
        );

        assert!(locations.contains(&("src/lib.rs".to_string(), 12)));
        assert!(locations.contains(&("tests/test_auth.py".to_string(), 52)));
        assert!(locations.contains(&("/path/to/file.ts".to_string(), 117)));
        assert!(locations.contains(&("src/main.rs".to_string(), 9)));
    }

    #[test]
    fn includes_nearby_excerpt_for_mentioned_file() {
        let root = temp_root("packet-excerpt");
        fs::create_dir_all(root.join("src/auth")).expect("source directory should be created");
        fs::write(
            root.join("src/auth/session.rs"),
            "line 1\nline 2\nline 3\nline 4\nline 5\n",
        )
        .expect("source should be written");

        let mentions = file_mentions(&root, &["error at src/auth/session.rs:3:5".to_string()]);

        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].path, "src/auth/session.rs");
        assert_eq!(mentions[0].line, 3);
        assert_eq!(
            mentions[0].excerpt,
            Some(LineExcerpt {
                start_line: 1,
                lines: vec![
                    "line 1".to_string(),
                    "line 2".to_string(),
                    "line 3".to_string(),
                    "line 4".to_string(),
                    "line 5".to_string(),
                ],
            })
        );

        fs::remove_dir_all(root).expect("test root should be removed");
    }

    #[test]
    fn loads_most_recent_failed_run_not_latest_success() {
        let root = temp_root("packet-last-failed");
        let db_path = root.join("haycut.sqlite3");
        let failed_run = root.join("failed-run");
        let success_run = root.join("success-run");
        write_run_artifacts(&failed_run, "failed", "cargo test auth", 101)
            .expect("failed run artifacts should be written");
        write_run_artifacts(&success_run, "success", "cargo test", 0)
            .expect("success run artifacts should be written");
        insert_packet_run(
            &db_path,
            "failed",
            "cargo test auth",
            101,
            "2026-07-07T15:29:00+00:00",
            &failed_run,
        )
        .expect("failed run should insert");
        insert_packet_run(
            &db_path,
            "success",
            "cargo test",
            0,
            "2026-07-07T15:30:00+00:00",
            &success_run,
        )
        .expect("success run should insert");

        let packet = load_last_failed_packet(&db_path).expect("last failed packet should load");

        assert!(packet.summary.iter().any(|line| line == "Run:      failed"));
        assert!(
            packet
                .summary
                .iter()
                .any(|line| line == "Command:  cargo test auth  exit code: 101")
        );

        fs::remove_dir_all(root).expect("test root should be removed");
    }

    #[test]
    fn renders_packet_with_command_files_excerpts_and_token_estimate() {
        let compact = CompactPacket {
            compactor: "native".to_string(),
            rtk_version: None,
            command: "cargo test auth".to_string(),
            exit_code: 101,
            duration_ms: 42,
            failed: true,
            stdout_artifact: "stdout.txt".to_string(),
            stderr_artifact: "stderr.txt".to_string(),
            compact_artifact: None,
            raw_stdout_tokens: 20,
            raw_stderr_tokens: 80,
            raw_tokens: 100,
            packet_tokens: 25,
            preserved_items: vec![PreservedItem {
                source: OutputSource::Stderr,
                kind: PreservedKind::ErrorLine,
                line: "tests/auth/session_test.rs:52 expected expired session".to_string(),
            }],
            omitted_items: Vec::new(),
            notes: Vec::new(),
            compact_text: None,
        };
        let packet = build_evidence_packet(
            PathBuf::from(".haycut/runs/run-1"),
            manifest_fixture("run-1", "cargo test auth", 101, "/tmp"),
            compact,
            vec![FileMention {
                path: "tests/auth/session_test.rs".to_string(),
                line: 52,
                excerpt: Some(LineExcerpt {
                    start_line: 50,
                    lines: vec![
                        "setup();".to_string(),
                        "let session = expired_session();".to_string(),
                        "assert!(validate_session(session).is_err());".to_string(),
                    ],
                }),
            }],
        );

        let rendered = packet.render(&token_config());

        assert!(rendered.contains("Command:  cargo test auth  exit code: 101"));
        assert!(rendered.contains("Included context:"));
        for item in &packet.items {
            assert!(!item.reason.trim().is_empty());
            let expected = format!(
                "  - source: {}  tokens: {}  reason: {}",
                item.source.label(),
                format_count(item.token_estimate),
                item.reason
            );
            assert!(
                rendered.contains(&expected),
                "missing included context row: {expected}"
            );
        }
        assert!(rendered.contains("tests/auth/session_test.rs:52"));
        assert!(rendered.contains("<excerpt lines 50-52>"));
        assert!(rendered.contains("assert!(validate_session(session).is_err());"));
        assert!(rendered.contains("packet tokens:"));
        assert!(rendered.contains("Budget:  soft: 40,000  hard: 80,000"));
        assert!(rendered.contains("Status: packet is within budget"));
    }

    #[test]
    fn detects_hard_budget_exceeded_for_packet() {
        let compact = CompactPacket {
            compactor: "native".to_string(),
            rtk_version: None,
            command: "cargo test auth".to_string(),
            exit_code: 101,
            duration_ms: 42,
            failed: true,
            stdout_artifact: "stdout.txt".to_string(),
            stderr_artifact: "stderr.txt".to_string(),
            compact_artifact: None,
            raw_stdout_tokens: 50,
            raw_stderr_tokens: 50,
            raw_tokens: 100,
            packet_tokens: 90_000,
            preserved_items: Vec::new(),
            omitted_items: Vec::new(),
            notes: Vec::new(),
            compact_text: None,
        };
        let packet = build_evidence_packet(
            PathBuf::from(".haycut/runs/run-1"),
            manifest_fixture("run-1", "cargo test auth", 101, "/tmp"),
            compact,
            Vec::new(),
        );

        assert!(packet.budget_usage(&token_config()).hard_error().is_some());
    }

    #[test]
    fn prunes_lower_priority_context_to_fit_budget() {
        let mut packet = EvidencePacket {
            title: "EVIDENCE PACKET".to_string(),
            summary: vec!["Run:      run-1".to_string()],
            items: vec![
                ContextItem {
                    kind: ContextKind::CommandSummary,
                    content: "cargo test exited with 101".to_string(),
                    source: SourceRef::Run {
                        id: "run-1".to_string(),
                    },
                    reason: "summarizes the captured command run".to_string(),
                    priority: Priority::High,
                    token_estimate: 6,
                },
                context_window_item("src/high.rs", 1, 20, Priority::High),
                context_window_item("src/medium.rs", 1, 15, Priority::Medium),
                context_window_item("src/low.rs", 1, 10, Priority::Low),
            ],
            omitted: Vec::new(),
            raw_token_estimate: 200,
            base_token_estimate: 10,
            token_estimate: 55,
            full_handles: Vec::new(),
        };

        packet.prune_to_budget(45);
        let rendered = packet.render(&token_config());

        assert_eq!(packet.token_estimate, 45);
        assert!(rendered.contains("source: src/high.rs lines 1-1"));
        assert!(rendered.contains("source: src/medium.rs lines 1-1"));
        assert!(
            !rendered.contains(
                "source: src/low.rs lines 1-1  tokens: 10  reason: nearby source context"
            )
        );
        assert!(rendered.contains("Omitted:"));
        assert!(rendered.contains(
            "source: src/low.rs lines 1-1  tokens: 10  reason: over budget; nearby source context"
        ));
        assert!(rendered.contains("packet tokens: 45"));
    }

    fn context_window_item(
        path: &str,
        start_line: usize,
        token_estimate: usize,
        priority: Priority,
    ) -> ContextItem {
        ContextItem {
            kind: ContextKind::CodeWindow,
            content: path.to_string(),
            source: SourceRef::CodeWindow {
                path: path.to_string(),
                start_line,
                end_line: start_line,
            },
            reason: "nearby source context".to_string(),
            priority,
            token_estimate,
        }
    }

    fn token_config() -> TokenConfig {
        TokenConfig {
            soft_budget: 40_000,
            hard_budget: 80_000,
        }
    }

    fn temp_root(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "haycut-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos()
        ))
    }

    fn write_run_artifacts(
        run_directory: &Path,
        id: &str,
        command: &str,
        exit_code: i32,
    ) -> io::Result<()> {
        fs::create_dir_all(run_directory)?;
        fs::write(run_directory.join("stdout.txt"), "")?;
        fs::write(
            run_directory.join("stderr.txt"),
            "error at tests/auth/session_test.rs:52:9\n",
        )?;
        let manifest = manifest_fixture(id, command, exit_code, "/tmp");
        let compact = CompactPacket {
            compactor: "native".to_string(),
            rtk_version: None,
            command: command.to_string(),
            exit_code,
            duration_ms: 42,
            failed: exit_code != 0,
            stdout_artifact: "stdout.txt".to_string(),
            stderr_artifact: "stderr.txt".to_string(),
            compact_artifact: None,
            raw_stdout_tokens: 0,
            raw_stderr_tokens: 10,
            raw_tokens: 10,
            packet_tokens: 5,
            preserved_items: Vec::new(),
            omitted_items: Vec::new(),
            notes: Vec::new(),
            compact_text: None,
        };

        fs::write(
            run_directory.join("run.json"),
            serde_json::to_string_pretty(&manifest).map_err(io::Error::other)?,
        )?;
        fs::write(
            run_directory.join("compact.json"),
            serde_json::to_string_pretty(&compact).map_err(io::Error::other)?,
        )
    }

    fn insert_packet_run(
        db_path: &Path,
        id: &str,
        command: &str,
        exit_code: i32,
        created_at: &str,
        run_directory: &Path,
    ) -> io::Result<()> {
        insert_run(
            db_path,
            &NewRun {
                id,
                command,
                cwd: "/tmp",
                exit_code: Some(exit_code),
                duration_ms: 42,
                raw_tokens: 10,
                packet_tokens: 5,
                created_at,
                artifacts: vec![
                    NewArtifact {
                        id: format!("{id}:run_manifest"),
                        kind: "run_manifest",
                        path: run_directory.join("run.json").display().to_string(),
                        estimated_tokens: None,
                    },
                    NewArtifact {
                        id: format!("{id}:compact_json"),
                        kind: "compact_json",
                        path: run_directory.join("compact.json").display().to_string(),
                        estimated_tokens: Some(5),
                    },
                ],
            },
        )
    }

    fn manifest_fixture(id: &str, command: &str, exit_code: i32, cwd: &str) -> RunManifest {
        RunManifest {
            id: id.to_string(),
            command: command.to_string(),
            args: Vec::new(),
            cwd: cwd.to_string(),
            exit_code,
            duration_ms: 42,
            stdout_bytes: 0,
            stderr_bytes: 0,
            estimated_raw_tokens: 10,
            raw_stdout_tokens_estimated: 0,
            raw_stderr_tokens_estimated: 10,
            created_at: Utc::now(),
            stdout: "stdout.txt".to_string(),
            stderr: "stderr.txt".to_string(),
            compact: "compact.json".to_string(),
        }
    }
}
