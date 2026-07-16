use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn absolute_git_path(manifest_dir: &Path, git_dir: &str) -> PathBuf {
    let path = Path::new(git_dir);
    if path.is_absolute() {
        path.to_owned()
    } else {
        manifest_dir.join(path)
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=.git/HEAD");
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let sha = git(&["rev-parse", "--verify", "HEAD"])
        .filter(|value| value.len() >= 8)
        .map(|value| value[..8].to_owned())
        .unwrap_or_else(|| "--------".to_owned());
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]) {
        let git_dir = absolute_git_path(&manifest_dir, &git_dir);
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join("packed-refs").display()
        );
        if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!(
                "cargo:rerun-if-changed={}",
                git_dir.join(reference).display()
            );
        }
    }
    println!("cargo:rustc-env=HAYCUT_BUILD_SHA={sha}");
}
