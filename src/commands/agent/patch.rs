use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{commands::task::PatchEdit, project_path};

#[derive(Debug)]
struct PlannedFile {
    path: PathBuf,
    relative_path: PathBuf,
    original: String,
    updated: String,
}

pub fn project_root() -> io::Result<PathBuf> {
    project_path::canonical_root()
}

pub fn preview_edits(root: &Path, edits: &[PatchEdit]) -> io::Result<String> {
    let files = plan_edits(root, edits)?;
    Ok(files
        .iter()
        .map(|file| {
            format!(
                "{}\n- {}\n+ {}",
                file.relative_path.display(),
                excerpt_change(&file.original, &file.updated, '-'),
                excerpt_change(&file.original, &file.updated, '+'),
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn apply_edits(root: &Path, edits: &[PatchEdit]) -> io::Result<String> {
    let files = plan_edits(root, edits)?;
    write_transaction(&files)?;
    Ok(format!(
        "applied {} edit(s) to {} file(s)",
        edits.len(),
        files.len()
    ))
}

fn plan_edits(root: &Path, edits: &[PatchEdit]) -> io::Result<Vec<PlannedFile>> {
    if edits.is_empty() {
        return Err(invalid_input("no edits proposed"));
    }

    let root = fs::canonicalize(root)?;
    let mut originals = BTreeMap::<PathBuf, (PathBuf, String)>::new();
    let mut anchors = HashSet::new();

    for edit in edits {
        if edit.path.trim().is_empty() {
            return Err(invalid_input("edit path is required"));
        }
        if edit.find.is_empty() {
            return Err(invalid_input(&format!(
                "{}: find text is required",
                edit.path
            )));
        }

        let (path, relative_path) = resolve_existing_path(&root, &edit.path)?;
        let anchor = (relative_path.clone(), edit.find.clone());
        if !anchors.insert(anchor) {
            return Err(invalid_input(&format!(
                "{}: duplicate edit anchor",
                relative_path.display()
            )));
        }

        let content = fs::read_to_string(&path)?;
        originals.entry(path).or_insert((relative_path, content));
    }

    let mut planned = Vec::with_capacity(originals.len());
    for (path, (relative_path, original)) in originals {
        let mut updated = original.clone();
        for edit in edits.iter().filter(|edit| {
            resolve_existing_path(&root, &edit.path)
                .map(|(candidate, _)| candidate == path)
                .unwrap_or(false)
        }) {
            let count = updated.matches(&edit.find).count();
            if count != 1 {
                return Err(invalid_input(&format!(
                    "{}: find text must occur exactly once (found {count})",
                    relative_path.display()
                )));
            }
            updated = updated.replacen(&edit.find, &edit.replace, 1);
        }
        reject_dirty_git_target(&root, &relative_path)?;
        planned.push(PlannedFile {
            path,
            relative_path,
            original,
            updated,
        });
    }

    Ok(planned)
}

pub fn resolve_existing_path(root: &Path, provided: &str) -> io::Result<(PathBuf, PathBuf)> {
    let path = project_path::resolve_existing(root, provided)?;
    Ok((path.absolute, path.relative))
}

fn reject_dirty_git_target(root: &Path, relative_path: &Path) -> io::Result<()> {
    let repository = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(root)
        .output()?;
    if !repository.status.success() {
        return Ok(());
    }

    let output = std::process::Command::new("git")
        .args(["status", "--porcelain", "--"])
        .arg(relative_path)
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            "unable to inspect Git working-tree status",
        ));
    }
    if !output.stdout.is_empty() {
        return Err(invalid_input(&format!(
            "{} has uncommitted changes; refusing to overwrite it",
            relative_path.display()
        )));
    }
    Ok(())
}

fn write_transaction(files: &[PlannedFile]) -> io::Result<()> {
    let mut temporary_files = Vec::with_capacity(files.len());
    for file in files {
        let temp = file.path.with_file_name(format!(
            ".{}.haycut-{}.tmp",
            file.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("patch"),
            Uuid::new_v4().simple()
        ));
        if let Err(error) = fs::write(&temp, &file.updated) {
            cleanup_temps(&temporary_files);
            return Err(error);
        }
        temporary_files.push(temp);
    }

    let mut committed: Vec<&PlannedFile> = Vec::new();
    for (file, temp) in files.iter().zip(&temporary_files) {
        if let Err(error) = fs::rename(temp, &file.path) {
            for restored in committed.iter().rev() {
                let _ = fs::write(&restored.path, &restored.original);
            }
            cleanup_temps(&temporary_files);
            return Err(error);
        }
        committed.push(file);
    }

    Ok(())
}

fn cleanup_temps(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

fn excerpt_change(original: &str, updated: &str, prefix: char) -> String {
    let (before, after) = match prefix {
        '-' => (original, updated),
        '+' => (updated, original),
        _ => unreachable!("only diff prefixes are used"),
    };
    before
        .lines()
        .find(|line| !after.lines().any(|other| other == *line))
        .unwrap_or(before)
        .chars()
        .take(180)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "haycut-patch-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("temporary root should be created");
        root
    }

    fn edit(path: &str, find: &str, replace: &str) -> PatchEdit {
        PatchEdit {
            path: path.to_string(),
            find: find.to_string(),
            replace: replace.to_string(),
        }
    }

    #[test]
    fn applies_one_unique_occurrence() {
        let root = temp_root("unique");
        fs::write(root.join("lib.rs"), "let value = 1;").unwrap();

        apply_edits(&root, &[edit("lib.rs", "value = 1", "value = 2")]).unwrap();

        assert_eq!(
            fs::read_to_string(root.join("lib.rs")).unwrap(),
            "let value = 2;"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_missing_and_ambiguous_anchors_without_writing() {
        let root = temp_root("anchors");
        let file = root.join("lib.rs");
        fs::write(&file, "x x").unwrap();

        for anchor in ["missing", "x"] {
            assert!(apply_edits(&root, &[edit("lib.rs", anchor, "y")]).is_err());
            assert_eq!(fs::read_to_string(&file).unwrap(), "x x");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_parent_and_absolute_outside_paths() {
        let root = temp_root("escape");
        let outside = temp_root("outside");
        fs::write(root.join("lib.rs"), "x").unwrap();
        fs::write(outside.join("outside.rs"), "x").unwrap();

        assert!(apply_edits(&root, &[edit("../outside.rs", "x", "y")]).is_err());
        assert!(
            apply_edits(
                &root,
                &[edit(
                    &outside.join("outside.rs").display().to_string(),
                    "x",
                    "y"
                )]
            )
            .is_err()
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escapes() {
        use std::os::unix::fs::symlink;

        let root = temp_root("symlink");
        let outside = temp_root("symlink-outside");
        fs::write(outside.join("outside.rs"), "x").unwrap();
        symlink(outside.join("outside.rs"), root.join("link.rs")).unwrap();

        assert!(apply_edits(&root, &[edit("link.rs", "x", "y")]).is_err());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn failed_multi_file_plan_leaves_every_file_unchanged() {
        let root = temp_root("transaction");
        let first = root.join("first.rs");
        let second = root.join("second.rs");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second second").unwrap();

        assert!(
            apply_edits(
                &root,
                &[
                    edit("first.rs", "first", "updated"),
                    edit("second.rs", "second", "updated"),
                ],
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(first).unwrap(), "first");
        assert_eq!(fs::read_to_string(second).unwrap(), "second second");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preview_does_not_write_files() {
        let root = temp_root("preview");
        let file = root.join("lib.rs");
        fs::write(&file, "x").unwrap();

        let preview = preview_edits(&root, &[edit("lib.rs", "x", "y")]).unwrap();

        assert!(preview.contains("lib.rs"));
        assert_eq!(fs::read_to_string(file).unwrap(), "x");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_dirty_patch_targets() {
        let root = temp_root("dirty");
        let file = root.join("lib.rs");
        fs::write(&file, "x").unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(init.success());
        let add = std::process::Command::new("git")
            .args(["add", "lib.rs"])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(add.success());
        let commit = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=HayCut test",
                "-c",
                "user.email=haycut@example.invalid",
                "commit",
                "-qm",
                "initial",
            ])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(commit.success());
        fs::write(&file, "user change").unwrap();

        let error = apply_edits(&root, &[edit("lib.rs", "user change", "agent change")])
            .expect_err("dirty target must be rejected");

        assert!(error.to_string().contains("uncommitted changes"));
        assert_eq!(fs::read_to_string(file).unwrap(), "user change");
        fs::remove_dir_all(root).unwrap();
    }
}
