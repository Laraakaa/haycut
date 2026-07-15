use std::{collections::HashSet, fs, io, path::Path, time::UNIX_EPOCH};

use chrono::{DateTime, Utc};
use ignore::{DirEntry, WalkBuilder};

use crate::{
    context::artifact::file_content_digest,
    store::{self, FileInventoryEntry, RUN_STORE_PATH},
    util::estimate_tokens,
};

pub const DEFAULT_MAX_FILE_SIZE_BYTES: u64 = 1_000_000;

const DEFAULT_SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    "coverage",
    ".next",
    ".cache",
];

#[derive(Debug, PartialEq, Eq)]
struct IndexSummary {
    indexed_files: usize,
    skipped_files: usize,
}

pub fn run(max_file_size: u64) -> i32 {
    match index_repository(Path::new("."), max_file_size, Path::new(RUN_STORE_PATH)) {
        Ok(summary) => {
            print_summary(&summary);
            0
        }
        Err(error) => {
            eprintln!("Error indexing repository: {error}");
            1
        }
    }
}

fn index_repository(root: &Path, max_file_size: u64, db_path: &Path) -> io::Result<IndexSummary> {
    let mut indexed_files = 0;
    let mut skipped_files = 0;
    let mut files = Vec::new();
    let skipped_directories = skipped_directories();
    let mut walker = WalkBuilder::new(root);
    walker.hidden(true);
    walker.filter_entry(move |entry| should_descend(entry, &skipped_directories));

    for result in walker.build() {
        let entry = match result {
            Ok(entry) => entry,
            Err(_) => {
                skipped_files += 1;
                continue;
            }
        };

        if entry.path() == root || is_dir(&entry) {
            continue;
        }

        if !is_file(&entry) {
            skipped_files += 1;
            continue;
        }

        let metadata = fs::metadata(entry.path())?;
        if metadata.len() > max_file_size {
            skipped_files += 1;
            continue;
        }

        files.push(inventory_entry(root, entry.path(), &metadata)?);
        indexed_files += 1;
    }

    store::replace_file_inventory(db_path, &files)?;

    Ok(IndexSummary {
        indexed_files,
        skipped_files,
    })
}

fn inventory_entry(
    root: &Path,
    path: &Path,
    metadata: &fs::Metadata,
) -> io::Result<FileInventoryEntry> {
    let contents = fs::read(path)?;
    let text = String::from_utf8_lossy(&contents);
    let line_count = count_lines(&text);
    let relative_path = path.strip_prefix(root).unwrap_or(path);

    Ok(FileInventoryEntry {
        path: normalize_path(relative_path),
        language: guess_language(path),
        byte_size: to_i64(metadata.len(), "file size")?,
        line_count: to_i64(line_count, "line count")?,
        estimated_tokens: to_i64(estimate_tokens(&contents), "token estimate")?,
        modified_at: modified_at(metadata),
        content_hash: content_hash(&contents),
    })
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn guess_language(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?;
    let language = match extension {
        "rs" => "Rust",
        "ts" => "TypeScript",
        "tsx" => "TSX",
        "js" => "JavaScript",
        "jsx" => "JSX",
        "py" => "Python",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "c" => "C",
        "h" => "C/C++ Header",
        "cc" | "cpp" | "cxx" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "md" => "Markdown",
        "json" => "JSON",
        "toml" => "TOML",
        "yaml" | "yml" => "YAML",
        "html" => "HTML",
        "css" => "CSS",
        "scss" => "SCSS",
        "sql" => "SQL",
        "sh" | "bash" | "zsh" => "Shell",
        _ => return None,
    };

    Some(language.to_string())
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    text.lines().count()
}

fn modified_at(metadata: &fs::Metadata) -> Option<String> {
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(UNIX_EPOCH).ok()?;
    let datetime = DateTime::<Utc>::from(UNIX_EPOCH + duration);

    Some(datetime.to_rfc3339())
}

fn content_hash(contents: &[u8]) -> String {
    file_content_digest(contents)
}

fn to_i64<T>(value: T, label: &str) -> io::Result<i64>
where
    i64: TryFrom<T>,
{
    i64::try_from(value).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} does not fit in SQLite integer"),
        )
    })
}

fn should_descend(entry: &DirEntry, skipped_directories: &HashSet<&'static str>) -> bool {
    if entry.depth() == 0 {
        return true;
    }

    if !is_dir(entry) {
        return true;
    }

    entry
        .file_name()
        .to_str()
        .map(|name| !skipped_directories.contains(name))
        .unwrap_or(true)
}

fn is_dir(entry: &DirEntry) -> bool {
    entry
        .file_type()
        .map(|file_type| file_type.is_dir())
        .unwrap_or_else(|| entry.path().is_dir())
}

fn is_file(entry: &DirEntry) -> bool {
    entry
        .file_type()
        .map(|file_type| file_type.is_file())
        .unwrap_or_else(|| entry.path().is_file())
}

fn skipped_directories() -> HashSet<&'static str> {
    DEFAULT_SKIPPED_DIRECTORIES.iter().copied().collect()
}

fn print_summary(summary: &IndexSummary) {
    println!("Indexed files: {}", summary.indexed_files);
    println!("Skipped files: {}", summary.skipped_files);
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn respects_gitignore_default_skips_and_max_file_size() {
        let root = temp_repo_root("index-skip");
        fs::create_dir_all(root.join("src")).expect("src should be created");
        fs::create_dir_all(root.join("target")).expect("target should be created");
        fs::create_dir_all(root.join(".venv/lib")).expect("hidden vendor dir should be created");
        fs::create_dir_all(root.join("node_modules/pkg")).expect("node_modules should be created");
        fs::write(root.join(".gitignore"), "ignored.txt\n").expect("gitignore should be written");
        fs::write(root.join("src/lib.rs"), "ok\n").expect("source should be written");
        fs::write(root.join("ignored.txt"), "ignored\n").expect("ignored file should be written");
        fs::write(root.join("target/debug.txt"), "build output\n")
            .expect("target file should be written");
        fs::write(root.join(".venv/lib/site.py"), "junk\n")
            .expect("hidden vendor file should be written");
        fs::write(root.join("node_modules/pkg/index.js"), "junk\n")
            .expect("vendor file should be written");
        fs::write(root.join("large.txt"), "123456").expect("large file should be written");

        let db_path = root.join(".haycut/haycut.sqlite3");
        let summary = index_repository(&root, 5, &db_path).expect("repository should index");
        let files = store::largest_files(&db_path, 10).expect("files should load");

        assert_eq!(summary.indexed_files, 1);
        assert_eq!(summary.skipped_files, 2);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(files[0].language.as_deref(), Some("Rust"));
        assert_eq!(files[0].byte_size, 3);
        assert_eq!(files[0].line_count, 1);
        assert_eq!(files[0].estimated_tokens, 0);
        assert!(!files[0].content_hash.is_empty());
        assert!(files[0].modified_at.is_some());

        fs::remove_dir_all(root).expect("test repo should be removed");
    }

    #[test]
    fn guesses_common_languages_from_extensions() {
        let cases = [
            ("src/main.rs", "Rust"),
            ("src/app.ts", "TypeScript"),
            ("src/app.tsx", "TSX"),
            ("src/app.js", "JavaScript"),
            ("src/app.jsx", "JSX"),
            ("scripts/tool.py", "Python"),
            ("cmd/server.go", "Go"),
            ("README.md", "Markdown"),
            ("Cargo.toml", "TOML"),
            ("package.json", "JSON"),
            ("config.yaml", "YAML"),
            ("config.yml", "YAML"),
        ];

        for (path, language) in cases {
            assert_eq!(guess_language(Path::new(path)).as_deref(), Some(language));
        }

        assert_eq!(guess_language(Path::new("unknown.file")).as_deref(), None);
    }

    fn temp_repo_root(label: &str) -> std::path::PathBuf {
        env::temp_dir().join(format!(
            "haycut-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos()
        ))
    }
}
