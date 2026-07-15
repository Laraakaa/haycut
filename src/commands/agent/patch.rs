use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{commands::task::PatchEdit, project_path};

pub fn project_root() -> io::Result<PathBuf> {
    project_path::canonical_root()
}

/// Content digest of a file, used for optimistic concurrency in two
/// complementary ways: `Delete`/`Rename`/digest-protected `Replace` edits
/// only apply if the file's current on-disk contents still match the digest
/// the edit was proposed against (Phase 8), and pre-existing dirty files may
/// still be edited if their current contents match the digest recorded when
/// the agent last inspected them as context (Phase 9, via `InspectedDigests`).
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FileDigest(pub String);

/// Hash `path`'s current contents. Missing files digest as `None` so
/// `Create`-style edits can distinguish "not there yet" from "changed".
pub fn digest_file(path: &Path) -> io::Result<Option<FileDigest>> {
    match fs::read(path) {
        Ok(content) => Ok(Some(FileDigest(blake3::hash(&content).to_hex().to_string()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Digests recorded by the agent when it read files as context, keyed by
/// path relative to the project root (as returned by `resolve_existing_path`).
/// Threaded into `preview_edits`/`apply_edits` so working-tree ownership
/// checks can tell "the user had pre-existing local changes before we
/// looked" apart from "the file changed after we inspected it".
pub type InspectedDigests = HashMap<String, FileDigest>;

/// A file mutation planned by `plan_edits`, ready to be previewed or
/// committed by `write_transaction`.
#[derive(Debug)]
enum PlannedOp {
    Write {
        path: PathBuf,
        relative_path: PathBuf,
        original: Option<String>,
        updated: String,
    },
    Delete {
        path: PathBuf,
        relative_path: PathBuf,
        original: String,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
        relative_from: PathBuf,
        relative_to: PathBuf,
    },
}

pub fn preview_edits(
    root: &Path,
    edits: &[PatchEdit],
    inspected: &InspectedDigests,
) -> io::Result<String> {
    let planned = plan_edits(root, edits, inspected)?;
    Ok(planned
        .iter()
        .map(describe_planned_op)
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn apply_edits(
    root: &Path,
    edits: &[PatchEdit],
    inspected: &InspectedDigests,
) -> io::Result<String> {
    let planned = plan_edits(root, edits, inspected)?;
    let file_count = planned.len();
    write_transaction(&planned)?;
    Ok(format!(
        "applied {} edit(s) to {file_count} file(s)",
        edits.len(),
    ))
}

fn describe_planned_op(op: &PlannedOp) -> String {
    match op {
        PlannedOp::Write { relative_path, original, updated, .. } => {
            let original = original.as_deref().unwrap_or("");
            format!(
                "{}\n- {}\n+ {}",
                relative_path.display(),
                excerpt_change(original, updated, '-'),
                excerpt_change(original, updated, '+'),
            )
        }
        PlannedOp::Delete { relative_path, .. } => {
            format!("{}\n- (file deleted)", relative_path.display())
        }
        PlannedOp::Rename { relative_from, relative_to, .. } => {
            format!(
                "{} -> {}\n(renamed, contents unchanged)",
                relative_from.display(),
                relative_to.display()
            )
        }
    }
}

fn check_digest(path: &Path, relative: &Path, expected: &FileDigest) -> io::Result<()> {
    let actual = digest_file(path)?;
    match actual {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) => Err(conflict_error(&format!(
            "{}: file has changed since it was inspected; refusing to apply",
            relative.display()
        ))),
        None => Err(invalid_input(&format!(
            "{}: file no longer exists; refusing to apply",
            relative.display()
        ))),
    }
}

fn plan_edits(
    root: &Path,
    edits: &[PatchEdit],
    inspected: &InspectedDigests,
) -> io::Result<Vec<PlannedOp>> {
    if edits.is_empty() {
        return Err(invalid_input("no edits proposed"));
    }

    let root = fs::canonicalize(root)?;

    // Replace edits: group find/replace anchors by target file so multiple
    // anchors in the same file are applied against the same read, same as
    // before this vocabulary expanded.
    let mut replace_originals = BTreeMap::<PathBuf, (PathBuf, String, bool)>::new();
    let mut anchors = HashSet::new();

    for edit in edits {
        let PatchEdit::Replace { path, find, expected_digest, .. } = edit else {
            continue;
        };
        if path.trim().is_empty() {
            return Err(invalid_input("edit path is required"));
        }
        if find.is_empty() {
            return Err(invalid_input(&format!("{path}: find text is required")));
        }

        let (resolved, relative_path) = resolve_existing_path(&root, path)?;
        let mut digest_checked = false;
        if let Some(expected) = expected_digest {
            check_digest(&resolved, &relative_path, expected)?;
            digest_checked = true;
        }
        let anchor = (relative_path.clone(), find.clone());
        if !anchors.insert(anchor) {
            return Err(invalid_input(&format!(
                "{}: duplicate edit anchor",
                relative_path.display()
            )));
        }

        let content = fs::read_to_string(&resolved)?;
        let entry = replace_originals
            .entry(resolved)
            .or_insert((relative_path, content, digest_checked));
        entry.2 = entry.2 || digest_checked;
    }

    let mut planned = Vec::with_capacity(edits.len());

    for (path, (relative_path, original, digest_checked)) in &replace_originals {
        let mut updated = original.clone();
        for edit in edits {
            let PatchEdit::Replace { path: edit_path, find, replace, .. } = edit else {
                continue;
            };
            match resolve_existing_path(&root, edit_path) {
                Ok((candidate, _)) if &candidate == path => {}
                _ => continue,
            }
            let count = updated.matches(find.as_str()).count();
            if count != 1 {
                return Err(invalid_input(&format!(
                    "{}: find text must occur exactly once (found {count})",
                    relative_path.display()
                )));
            }
            updated = updated.replacen(find.as_str(), replace, 1);
        }
        // A caller-supplied `expected_digest` already proves the file
        // matches the version the patch was planned against, which is a
        // strictly stronger guarantee than the working-tree dirtiness
        // fallback below — skip the redundant git check in that case.
        if !digest_checked {
            let inspected_digest = inspected.get(&relative_path.to_string_lossy().to_string());
            reject_dirty_git_target(&root, relative_path, inspected_digest)?;
        }
        planned.push(PlannedOp::Write {
            path: path.clone(),
            relative_path: relative_path.clone(),
            original: Some(original.clone()),
            updated,
        });
    }

    for edit in edits {
        match edit {
            PatchEdit::Replace { .. } => {} // handled above
            PatchEdit::Create { path, content, overwrite } => {
                if path.trim().is_empty() {
                    return Err(invalid_input("edit path is required"));
                }
                let (resolved, relative_path) = resolve_creatable_path(&root, path)?;
                if resolved.exists() && !overwrite {
                    return Err(invalid_input(&format!(
                        "{}: file already exists; set `overwrite` to replace it",
                        relative_path.display()
                    )));
                }
                if resolved.exists() {
                    let inspected_digest = inspected.get(&relative_path.to_string_lossy().to_string());
                    reject_dirty_git_target(&root, &relative_path, inspected_digest)?;
                }
                planned.push(PlannedOp::Write {
                    path: resolved,
                    relative_path,
                    original: None,
                    updated: content.clone(),
                });
            }
            PatchEdit::Delete { path, expected_digest } => {
                if path.trim().is_empty() {
                    return Err(invalid_input("edit path is required"));
                }
                let (resolved, relative_path) = resolve_existing_path(&root, path)?;
                check_digest(&resolved, &relative_path, expected_digest)?;
                let original = fs::read_to_string(&resolved)?;
                planned.push(PlannedOp::Delete {
                    path: resolved,
                    relative_path,
                    original,
                });
            }
            PatchEdit::Rename { from, to, expected_digest } => {
                if from.trim().is_empty() || to.trim().is_empty() {
                    return Err(invalid_input("rename requires both `from` and `to` paths"));
                }
                let (resolved_from, relative_from) = resolve_existing_path(&root, from)?;
                check_digest(&resolved_from, &relative_from, expected_digest)?;
                let (resolved_to, relative_to) = resolve_creatable_path(&root, to)?;
                if resolved_to.exists() {
                    return Err(invalid_input(&format!(
                        "{}: rename target already exists",
                        relative_to.display()
                    )));
                }
                planned.push(PlannedOp::Rename {
                    from: resolved_from,
                    to: resolved_to,
                    relative_from,
                    relative_to,
                });
            }
        }
    }

    Ok(planned)
}

pub fn resolve_existing_path(root: &Path, provided: &str) -> io::Result<(PathBuf, PathBuf)> {
    let path = project_path::resolve_existing(root, provided)?;
    Ok((path.absolute, path.relative))
}

pub fn resolve_creatable_path(root: &Path, provided: &str) -> io::Result<(PathBuf, PathBuf)> {
    let path = project_path::resolve_creatable(root, provided)?;
    Ok((path.absolute, path.relative))
}

/// Optimistic-concurrency check for a single edit target that has no
/// caller-supplied `expected_digest` of its own. A clean working tree is
/// always fine. A dirty one is only fine if the agent recorded a digest for
/// this path when it read the file as context *and* the file's current
/// content still matches that digest — i.e. the dirtiness predates the
/// agent's inspection, not a change that happened after. Anything else
/// (never inspected, or changed since inspection) is refused as a
/// recoverable conflict (see `is_conflict`) rather than a hard error, so the
/// caller can report it as a planner observation and return to planning
/// instead of silently clobbering work.
fn reject_dirty_git_target(
    root: &Path,
    relative_path: &Path,
    inspected_digest: Option<&FileDigest>,
) -> io::Result<()> {
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
    if output.stdout.is_empty() {
        return Ok(());
    }

    let Some(inspected_digest) = inspected_digest else {
        return Err(invalid_input(&format!(
            "{} has uncommitted changes and was never inspected by the agent; refusing to overwrite it",
            relative_path.display()
        )));
    };

    let current_digest = digest_file(&root.join(relative_path))?;
    if current_digest.as_ref() == Some(inspected_digest) {
        // Pre-existing user changes the agent already saw when it read the
        // file — safe to proceed under optimistic concurrency.
        return Ok(());
    }

    Err(conflict_error(&format!(
        "{} changed after the agent inspected it (working tree no longer matches the version the patch was planned against); refusing to apply a stale patch",
        relative_path.display()
    )))
}

/// A working-tree ownership conflict distinct from other validation errors:
/// callers should treat this as recoverable (return to planning) rather than
/// a hard failure. Uses `WouldBlock` as a marker kind since nothing else in
/// this module's error surface uses it.
fn conflict_error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, message.to_string())
}

/// Whether `error` represents a recoverable working-tree ownership conflict
/// (as opposed to a structural patch error like a missing anchor).
pub fn is_conflict(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
}

fn temp_sibling(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        ".{}.haycut-{}.tmp",
        path.file_name().and_then(|name| name.to_str()).unwrap_or("patch"),
        Uuid::new_v4().simple()
    ))
}

/// Commit every planned op transactionally: stage writes into temp files
/// first, then rename/delete/move only once every stage has succeeded, and
/// roll back any already-committed op if a later one fails.
fn write_transaction(ops: &[PlannedOp]) -> io::Result<()> {
    let mut temporary_files = Vec::with_capacity(ops.len());
    for op in ops {
        if let PlannedOp::Write { path, updated, .. } = op {
            let temp = temp_sibling(path);
            if let Err(error) = fs::write(&temp, updated) {
                cleanup_temps(&temporary_files);
                return Err(error);
            }
            temporary_files.push(Some(temp));
        } else {
            temporary_files.push(None);
        }
    }

    let mut committed: Vec<&PlannedOp> = Vec::new();
    for (op, temp) in ops.iter().zip(&temporary_files) {
        let result = match op {
            PlannedOp::Write { path, .. } => {
                fs::rename(temp.as_ref().expect("write op stages a temp file"), path)
            }
            PlannedOp::Delete { path, .. } => fs::remove_file(path),
            PlannedOp::Rename { from, to, .. } => fs::rename(from, to),
        };
        if let Err(error) = result {
            rollback(&committed);
            cleanup_temps(&temporary_files);
            return Err(error);
        }
        committed.push(op);
    }

    Ok(())
}

/// Best-effort restoration of already-committed ops when a later op in the
/// same transaction fails, so a partially-applied patch never lands.
fn rollback(committed: &[&PlannedOp]) {
    for op in committed.iter().rev() {
        match op {
            PlannedOp::Write { path, original: Some(original), .. } => {
                let _ = fs::write(path, original);
            }
            PlannedOp::Write { path, original: None, .. } => {
                let _ = fs::remove_file(path);
            }
            PlannedOp::Delete { path, original, .. } => {
                let _ = fs::write(path, original);
            }
            PlannedOp::Rename { from, to, .. } => {
                let _ = fs::rename(to, from);
            }
        }
    }
}

fn cleanup_temps(paths: &[Option<PathBuf>]) {
    for path in paths.iter().flatten() {
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
        PatchEdit::Replace {
            path: path.to_string(),
            find: find.to_string(),
            replace: replace.to_string(),
            expected_digest: None,
        }
    }

    fn no_digests() -> InspectedDigests {
        HashMap::new()
    }

    fn init_git_repo(root: &Path) {
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(init.success());
        let add = std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(root)
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
            .current_dir(root)
            .status()
            .unwrap();
        assert!(commit.success());
    }

    #[test]
    fn applies_one_unique_occurrence() {
        let root = temp_root("unique");
        fs::write(root.join("lib.rs"), "let value = 1;").unwrap();

        apply_edits(&root, &[edit("lib.rs", "value = 1", "value = 2")], &no_digests()).unwrap();

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
            assert!(apply_edits(&root, &[edit("lib.rs", anchor, "y")], &no_digests()).is_err());
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

        assert!(apply_edits(&root, &[edit("../outside.rs", "x", "y")], &no_digests()).is_err());
        assert!(
            apply_edits(
                &root,
                &[edit(
                    &outside.join("outside.rs").display().to_string(),
                    "x",
                    "y"
                )],
                &no_digests(),
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

        assert!(apply_edits(&root, &[edit("link.rs", "x", "y")], &no_digests()).is_err());
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
                &no_digests(),
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

        let preview = preview_edits(&root, &[edit("lib.rs", "x", "y")], &no_digests()).unwrap();

        assert!(preview.contains("lib.rs"));
        assert_eq!(fs::read_to_string(file).unwrap(), "x");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_never_inspected_dirty_patch_targets() {
        let root = temp_root("dirty");
        let file = root.join("lib.rs");
        fs::write(&file, "x").unwrap();
        init_git_repo(&root);
        fs::write(&file, "user change").unwrap();

        let error = apply_edits(
            &root,
            &[edit("lib.rs", "user change", "agent change")],
            &no_digests(),
        )
        .expect_err("dirty target never inspected by the agent must be rejected");

        assert!(error.to_string().contains("uncommitted changes"));
        assert!(!is_conflict(&error));
        assert_eq!(fs::read_to_string(file).unwrap(), "user change");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn allows_pre_existing_dirty_file_the_agent_already_inspected() {
        let root = temp_root("dirty-inspected");
        let file = root.join("lib.rs");
        fs::write(&file, "x").unwrap();
        init_git_repo(&root);
        // A user edit happened before the agent ever looked at the file.
        fs::write(&file, "user change").unwrap();
        let inspected_digest = digest_file(&file).unwrap().expect("file exists");
        let mut digests = no_digests();
        digests.insert("lib.rs".to_string(), inspected_digest);

        apply_edits(
            &root,
            &[edit("lib.rs", "user change", "agent change")],
            &digests,
        )
        .expect("pre-existing dirty file inspected by the agent should be allowed");

        assert_eq!(fs::read_to_string(&file).unwrap(), "agent change");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_to_apply_over_a_file_changed_after_inspection() {
        let root = temp_root("dirty-stale");
        let file = root.join("lib.rs");
        fs::write(&file, "x").unwrap();
        init_git_repo(&root);
        fs::write(&file, "user change").unwrap();
        let inspected_digest = digest_file(&file).unwrap().expect("file exists");
        // The file changes again after the agent recorded its digest — this
        // must be a recoverable conflict, not a silent overwrite.
        fs::write(&file, "yet another user change").unwrap();
        let mut digests = no_digests();
        digests.insert("lib.rs".to_string(), inspected_digest);

        let error = apply_edits(
            &root,
            &[edit("lib.rs", "yet another user change", "agent change")],
            &digests,
        )
        .expect_err("file changed after inspection must be rejected");

        assert!(error.to_string().contains("changed after the agent inspected it"));
        assert!(is_conflict(&error));
        assert_eq!(fs::read_to_string(&file).unwrap(), "yet another user change");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn creates_new_file() {
        let root = temp_root("create");

        apply_edits(
            &root,
            &[PatchEdit::Create {
                path: "new.rs".to_string(),
                content: "fn main() {}".to_string(),
                overwrite: false,
            }],
            &no_digests(),
        )
        .unwrap();

        assert_eq!(fs::read_to_string(root.join("new.rs")).unwrap(), "fn main() {}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_rejects_existing_file_without_overwrite() {
        let root = temp_root("create-exists");
        fs::write(root.join("existing.rs"), "old").unwrap();

        let error = apply_edits(
            &root,
            &[PatchEdit::Create {
                path: "existing.rs".to_string(),
                content: "new".to_string(),
                overwrite: false,
            }],
            &no_digests(),
        )
        .expect_err("create must refuse to clobber an existing file");

        assert!(error.to_string().contains("already exists"));
        assert_eq!(fs::read_to_string(root.join("existing.rs")).unwrap(), "old");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_with_overwrite_replaces_existing_file() {
        let root = temp_root("create-overwrite");
        fs::write(root.join("existing.rs"), "old").unwrap();

        apply_edits(
            &root,
            &[PatchEdit::Create {
                path: "existing.rs".to_string(),
                content: "new".to_string(),
                overwrite: true,
            }],
            &no_digests(),
        )
        .unwrap();

        assert_eq!(fs::read_to_string(root.join("existing.rs")).unwrap(), "new");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deletes_file_when_digest_matches() {
        let root = temp_root("delete");
        let file = root.join("gone.rs");
        fs::write(&file, "content").unwrap();
        let digest = digest_file(&file).unwrap().unwrap();

        apply_edits(
            &root,
            &[PatchEdit::Delete { path: "gone.rs".to_string(), expected_digest: digest }],
            &no_digests(),
        )
        .unwrap();

        assert!(!file.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delete_rejects_stale_digest() {
        let root = temp_root("delete-stale");
        let file = root.join("changed.rs");
        fs::write(&file, "content").unwrap();
        let stale_digest = digest_file(&file).unwrap().unwrap();
        fs::write(&file, "different content").unwrap();

        let error = apply_edits(
            &root,
            &[PatchEdit::Delete { path: "changed.rs".to_string(), expected_digest: stale_digest }],
            &no_digests(),
        )
        .expect_err("stale digest must be rejected");

        assert!(error.to_string().contains("changed"));
        assert!(is_conflict(&error));
        assert!(file.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn renames_file_when_digest_matches() {
        let root = temp_root("rename");
        let from = root.join("old.rs");
        fs::write(&from, "content").unwrap();
        let digest = digest_file(&from).unwrap().unwrap();

        apply_edits(
            &root,
            &[PatchEdit::Rename {
                from: "old.rs".to_string(),
                to: "new.rs".to_string(),
                expected_digest: digest,
            }],
            &no_digests(),
        )
        .unwrap();

        assert!(!from.exists());
        assert_eq!(fs::read_to_string(root.join("new.rs")).unwrap(), "content");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rename_rejects_existing_target() {
        let root = temp_root("rename-exists");
        let from = root.join("old.rs");
        fs::write(&from, "content").unwrap();
        fs::write(root.join("new.rs"), "already here").unwrap();
        let digest = digest_file(&from).unwrap().unwrap();

        let error = apply_edits(
            &root,
            &[PatchEdit::Rename {
                from: "old.rs".to_string(),
                to: "new.rs".to_string(),
                expected_digest: digest,
            }],
            &no_digests(),
        )
        .expect_err("rename must refuse to clobber an existing target");

        assert!(error.to_string().contains("already exists"));
        assert!(from.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
