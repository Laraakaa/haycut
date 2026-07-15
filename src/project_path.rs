use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

pub struct ProjectPath {
    pub absolute: PathBuf,
    pub relative: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathRequirement {
    ExistingFile,
    #[allow(dead_code)]
    CreatableFile,
}

pub fn canonical_root() -> io::Result<PathBuf> {
    fs::canonicalize(".")
}

pub fn resolve_existing(root: &Path, provided: &str) -> io::Result<ProjectPath> {
    resolve(root, provided, PathRequirement::ExistingFile)
}

#[allow(dead_code)]
pub fn resolve_creatable(root: &Path, provided: &str) -> io::Result<ProjectPath> {
    resolve(root, provided, PathRequirement::CreatableFile)
}

fn resolve(root: &Path, provided: &str, requirement: PathRequirement) -> io::Result<ProjectPath> {
    let root = fs::canonicalize(root)?;
    let path = Path::new(provided);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        if path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(invalid_input(
                "path must not contain parent-directory traversal",
            ));
        }
        root.join(path)
    };
    let absolute = match requirement {
        PathRequirement::ExistingFile => fs::canonicalize(candidate)?,
        PathRequirement::CreatableFile => {
            let parent = candidate
                .parent()
                .ok_or_else(|| invalid_input("path has no parent directory"))?;
            let canonical_parent = fs::canonicalize(parent)?;
            let file_name = candidate
                .file_name()
                .ok_or_else(|| invalid_input("path must name a file"))?;
            canonical_parent.join(file_name)
        }
    };
    let relative = absolute
        .strip_prefix(&root)
        .map_err(|_| invalid_input("path resolves outside the project root"))?
        .to_path_buf();
    if requirement == PathRequirement::ExistingFile && !absolute.is_file() {
        return Err(invalid_input(&format!(
            "{} is not a readable file",
            relative.display()
        )));
    }

    Ok(ProjectPath { absolute, relative })
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "haycut-project-path-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("temporary root should be created");
        root
    }

    #[test]
    fn resolves_relative_paths_to_stable_relative_paths() {
        let root = temp_root("relative");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();

        let path = resolve_existing(&root, "src/lib.rs").unwrap();

        assert_eq!(path.relative, PathBuf::from("src/lib.rs"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_parent_and_absolute_outside_paths() {
        let root = temp_root("root");
        let outside = temp_root("outside");
        fs::write(root.join("lib.rs"), "").unwrap();
        fs::write(outside.join("lib.rs"), "").unwrap();

        assert!(resolve_existing(&root, "../lib.rs").is_err());
        assert!(resolve_existing(&root, &outside.join("lib.rs").display().to_string()).is_err());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn resolves_creatable_paths_without_requiring_the_file() {
        let root = temp_root("create");
        fs::create_dir_all(root.join("src")).unwrap();

        let path = resolve_creatable(&root, "src/new.rs").unwrap();

        assert_eq!(path.relative, PathBuf::from("src/new.rs"));
        assert!(!path.absolute.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escapes() {
        use std::os::unix::fs::symlink;

        let root = temp_root("root");
        let outside = temp_root("outside");
        fs::write(outside.join("lib.rs"), "").unwrap();
        symlink(outside.join("lib.rs"), root.join("link.rs")).unwrap();

        assert!(resolve_existing(&root, "link.rs").is_err());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
