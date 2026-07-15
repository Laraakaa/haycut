use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

use ignore::{DirEntry, WalkBuilder};

use crate::{
    project_path,
    symbols::{Symbol, SymbolLanguage, parse_symbols},
    util::estimate_tokens,
};

const HUGE_SYMBOL_TOKEN_WARNING: usize = 2_000;
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

#[derive(Debug)]
pub struct SymbolMatch {
    pub path: String,
    pub symbol: Symbol,
    pub code: String,
    pub estimated_tokens: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct SymbolTarget {
    path: Option<String>,
    name: String,
}

pub fn run(target: String) -> i32 {
    match read_symbol(Path::new("."), &target) {
        Ok(symbol) => {
            print_symbol(&symbol);
            0
        }
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            eprintln!("Error: {error}");
            2
        }
        Err(error) => {
            eprintln!("Error reading symbol: {error}");
            1
        }
    }
}

pub fn read_symbol(root: &Path, target: &str) -> io::Result<SymbolMatch> {
    let root = fs::canonicalize(root)?;
    let mut target = parse_target(target);
    if let Some(path) = target.path.as_deref() {
        target.path = Some(
            project_path::resolve_existing(&root, path)?
                .relative
                .to_string_lossy()
                .into_owned(),
        );
    }
    let matches = find_symbol_matches(&root, &target)?;

    match matches.len() {
        0 => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("symbol {} was not found", target.name),
        )),
        1 => Ok(matches.into_iter().next().expect("one match should exist")),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            duplicate_symbol_message(&target.name, &matches),
        )),
    }
}

fn find_symbol_matches(root: &Path, target: &SymbolTarget) -> io::Result<Vec<SymbolMatch>> {
    let mut matches = Vec::new();
    for (path, language) in symbol_files(root) {
        let relative_path = normalize_path(path.strip_prefix(root).unwrap_or(&path));
        if let Some(target_path) = target.path.as_deref()
            && relative_path != target_path
        {
            continue;
        }

        let source = fs::read_to_string(&path)?;
        let symbols = parse_symbols(&source, language)?;
        for symbol in symbols
            .into_iter()
            .filter(|symbol| symbol.name == target.name)
        {
            let code = source
                .get(symbol.start_byte..symbol.end_byte)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "symbol byte range is invalid")
                })?
                .to_string();
            let estimated_tokens = estimate_tokens(code.as_bytes());
            matches.push(SymbolMatch {
                path: relative_path.clone(),
                symbol,
                code,
                estimated_tokens,
            });
        }
    }

    Ok(matches)
}

pub(crate) fn symbol_files(root: &Path) -> Vec<(PathBuf, SymbolLanguage)> {
    let skipped_directories = skipped_directories();
    let mut walker = WalkBuilder::new(root);
    walker.hidden(false);
    walker.require_git(false);
    walker.add_custom_ignore_filename(".gitignore");
    walker.filter_entry(move |entry| should_descend(entry, &skipped_directories));

    walker
        .build()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .map(|file_type| file_type.is_file())
                .unwrap_or_else(|| entry.path().is_file())
        })
        .filter_map(|entry| {
            SymbolLanguage::from_path(entry.path()).map(|language| (entry.into_path(), language))
        })
        .collect()
}

fn parse_target(target: &str) -> SymbolTarget {
    if let Some((path, name)) = target.rsplit_once("::")
        && SymbolLanguage::from_path(Path::new(path)).is_some()
        && !name.is_empty()
    {
        return SymbolTarget {
            path: Some(path.to_string()),
            name: name.to_string(),
        };
    }

    SymbolTarget {
        path: None,
        name: target.to_string(),
    }
}

fn duplicate_symbol_message(name: &str, matches: &[SymbolMatch]) -> String {
    let mut message = format!("symbol {name} is ambiguous; use path::name. Candidates:");
    for item in matches {
        message.push_str(&format!(
            "\n  {}::{} lines {}-{}",
            item.path, item.symbol.name, item.symbol.start_line, item.symbol.end_line
        ));
    }

    message
}

fn print_symbol(symbol: &SymbolMatch) {
    println!("Symbol: {}", symbol.symbol.name);
    println!("File: {}", symbol.path);
    println!(
        "Lines: {}-{}",
        symbol.symbol.start_line, symbol.symbol.end_line
    );
    println!("Estimated tokens: {}", symbol.estimated_tokens);
    if symbol.estimated_tokens > HUGE_SYMBOL_TOKEN_WARNING {
        println!(
            "Warning: symbol is large; printing more than {} estimated tokens",
            HUGE_SYMBOL_TOKEN_WARNING
        );
    }
    println!("<code>{}</code>", symbol.code);
}

fn should_descend(entry: &DirEntry, skipped_directories: &HashSet<&'static str>) -> bool {
    if entry.depth() == 0 {
        return true;
    }

    if !entry.path().is_dir() {
        return true;
    }

    entry
        .file_name()
        .to_str()
        .map(|name| !skipped_directories.contains(name))
        .unwrap_or(true)
}

fn skipped_directories() -> HashSet<&'static str> {
    DEFAULT_SKIPPED_DIRECTORIES.iter().copied().collect()
}

pub(crate) fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn reads_exact_symbol_slice_by_path_and_name() {
        let root = temp_repo_root("read-symbol-exact");
        let src = root.join("src");
        fs::create_dir_all(&src).expect("src should be created");
        fs::write(
            src.join("main.rs"),
            "fn helper() {}\n\nfn main() {\n    println!(\"hi\");\n}\n",
        )
        .expect("source should be written");

        let symbol = read_symbol(&root, "src/main.rs::main").expect("symbol should read");

        assert_eq!(symbol.path, "src/main.rs");
        assert_eq!(symbol.symbol.name, "main");
        assert_eq!(symbol.symbol.start_line, 3);
        assert_eq!(symbol.symbol.end_line, 5);
        assert_eq!(symbol.code, "fn main() {\n    println!(\"hi\");\n}");

        fs::remove_dir_all(root).expect("test repo should be removed");
    }

    #[test]
    fn lists_candidates_for_duplicate_symbol_names() {
        let root = temp_repo_root("read-symbol-duplicates");
        fs::create_dir_all(root.join("src/bin")).expect("src should be created");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("main should be written");
        fs::write(root.join("src/bin/tool.rs"), "fn main() {}\n").expect("tool should be written");

        let error = read_symbol(&root, "main").expect_err("duplicate symbol should fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("src/main.rs::main"));
        assert!(error.to_string().contains("src/bin/tool.rs::main"));

        fs::remove_dir_all(root).expect("test repo should be removed");
    }

    #[test]
    fn respects_gitignore_when_finding_symbols() {
        let root = temp_repo_root("read-symbol-ignore");
        fs::create_dir_all(root.join("src")).expect("src should be created");
        fs::create_dir_all(root.join("ignored")).expect("ignored dir should be created");
        fs::write(root.join(".gitignore"), "ignored/\n").expect("gitignore should be written");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("main should be written");
        fs::write(root.join("ignored/main.rs"), "fn main() {}\n")
            .expect("ignored should be written");

        let symbol = read_symbol(&root, "main").expect("ignored duplicate should not count");

        assert_eq!(symbol.path, "src/main.rs");

        fs::remove_dir_all(root).expect("test repo should be removed");
    }

    #[test]
    fn reads_typescript_symbols() {
        let root = temp_repo_root("read-symbol-typescript");
        let src = root.join("src");
        fs::create_dir_all(&src).expect("src should be created");
        fs::write(
            src.join("session.ts"),
            "export function buildSession() {\n    return {};\n}\n",
        )
        .expect("TypeScript source should be written");

        let symbol =
            read_symbol(&root, "src/session.ts::buildSession").expect("symbol should read");

        assert_eq!(symbol.path, "src/session.ts");
        assert_eq!(symbol.symbol.name, "buildSession");
        assert_eq!(symbol.symbol.start_line, 1);
        assert_eq!(symbol.symbol.end_line, 3);
        assert_eq!(symbol.code, "function buildSession() {\n    return {};\n}");

        fs::remove_dir_all(root).expect("test repo should be removed");
    }

    #[test]
    fn reads_python_methods() {
        let root = temp_repo_root("read-symbol-python");
        let src = root.join("src");
        fs::create_dir_all(&src).expect("src should be created");
        fs::write(
            src.join("session.py"),
            "class Session:\n    def refresh(self):\n        return True\n",
        )
        .expect("Python source should be written");

        let symbol = read_symbol(&root, "src/session.py::refresh").expect("symbol should read");

        assert_eq!(symbol.path, "src/session.py");
        assert_eq!(symbol.symbol.name, "refresh");
        assert_eq!(symbol.symbol.start_line, 2);
        assert_eq!(symbol.symbol.end_line, 3);
        assert_eq!(symbol.code, "def refresh(self):\n        return True");

        fs::remove_dir_all(root).expect("test repo should be removed");
    }

    #[test]
    fn parses_path_qualified_targets() {
        assert_eq!(
            parse_target("src/main.rs::main"),
            SymbolTarget {
                path: Some("src/main.rs".to_string()),
                name: "main".to_string(),
            }
        );
        assert_eq!(
            parse_target("src/session.ts::buildSession"),
            SymbolTarget {
                path: Some("src/session.ts".to_string()),
                name: "buildSession".to_string(),
            }
        );
        assert_eq!(
            parse_target("src/session.py::refresh"),
            SymbolTarget {
                path: Some("src/session.py".to_string()),
                name: "refresh".to_string(),
            }
        );
        assert_eq!(
            parse_target("main"),
            SymbolTarget {
                path: None,
                name: "main".to_string(),
            }
        );
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
