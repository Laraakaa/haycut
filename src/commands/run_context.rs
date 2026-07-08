use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{commands::trace::RunManifest, compactor::CompactPacket, store};

/// Shared context loaded from a stored run: manifest, compact packet, and run
/// directory.  Centralises the repeated "find last run → load manifest → load
/// compact" pattern used by `packet`, `report`, and `suggest`.
pub struct RunContext {
    pub run_directory: PathBuf,
    pub manifest: RunManifest,
    pub compact: CompactPacket,
}

impl RunContext {
    /// Load the most recent run (any exit code).
    pub fn load_last(db_path: &Path) -> io::Result<Self> {
        Self::load(db_path, false)
    }

    /// Load the most recent failed run (non-zero exit code).
    pub fn load_last_failed(db_path: &Path) -> io::Result<Self> {
        Self::load(db_path, true)
    }

    fn load(db_path: &Path, failed_only: bool) -> io::Result<Self> {
        let stored_run = if failed_only {
            store::latest_failed_run(db_path)?
        } else {
            store::latest_run(db_path)?
        };

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

        Ok(Self {
            run_directory,
            manifest,
            compact,
        })
    }

    /// Read stdout as lossy UTF-8, propagating I/O errors.
    pub fn read_stdout_lossy(&self) -> io::Result<String> {
        let bytes = fs::read(self.run_directory.join(&self.manifest.stdout))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Read stderr as lossy UTF-8, propagating I/O errors.
    pub fn read_stderr_lossy(&self) -> io::Result<String> {
        let bytes = fs::read(self.run_directory.join(&self.manifest.stderr))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}
