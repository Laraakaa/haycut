use std::{fs, io, path::Path};

use chrono::{DateTime, Utc};

use crate::{
    commands::trace::RunManifest, compactor::CompactPacket, evidence::EvidencePacket, store,
};

/// Shared context loaded from a stored run: manifest, compact packet, evidence
/// packet, and run directory.  Centralises the repeated "find last run → load
/// manifest → load compact" pattern used by `packet`, `report`, and `suggest`.
pub struct RunContext {
    pub manifest: RunManifest,
    pub compact: CompactPacket,
    pub evidence: EvidencePacket,
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

        let compact: CompactPacket =
            serde_json::from_str(&stored_run.compact_json).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid compact packet for run {}: {error}", stored_run.id),
                )
            })?;

        let evidence: EvidencePacket =
            serde_json::from_str(&stored_run.evidence_json).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid evidence packet for run {}: {error}", stored_run.id),
                )
            })?;

        Ok(Self {
            manifest: manifest_from_stored(&stored_run)?,
            compact,
            evidence,
        })
    }

    /// Read stdout as lossy UTF-8, propagating I/O errors.
    pub fn read_stdout_lossy(&self) -> io::Result<String> {
        let bytes = fs::read(&self.manifest.stdout)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Read stderr as lossy UTF-8, propagating I/O errors.
    pub fn read_stderr_lossy(&self) -> io::Result<String> {
        let bytes = fs::read(&self.manifest.stderr)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn manifest_from_stored(stored: &store::StoredRun) -> io::Result<RunManifest> {
    let args: Vec<String> = serde_json::from_str(&stored.args_json).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid args JSON for run {}: {error}", stored.id),
        )
    })?;
    let created_at = DateTime::parse_from_rfc3339(&stored.created_at)
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid created_at for run {}: {error}", stored.id),
            )
        })?
        .with_timezone(&Utc);

    Ok(RunManifest {
        id: stored.id.clone(),
        command: stored.command.clone(),
        args,
        cwd: stored.cwd.clone(),
        exit_code: stored.exit_code.unwrap_or_default(),
        duration_ms: stored.duration_ms.unwrap_or_default() as u128,
        stdout_bytes: stored.stdout_bytes.unwrap_or_default() as usize,
        stderr_bytes: stored.stderr_bytes.unwrap_or_default() as usize,
        estimated_raw_tokens: stored.raw_tokens.unwrap_or_default() as usize,
        raw_stdout_tokens_estimated: stored.raw_stdout_tokens.unwrap_or_default() as usize,
        raw_stderr_tokens_estimated: stored.raw_stderr_tokens.unwrap_or_default() as usize,
        created_at,
        stdout: stored.stdout_path.clone(),
        stderr: stored.stderr_path.clone(),
        compact: "sqlite:compact_json".to_string(),
        evidence: "sqlite:evidence_json".to_string(),
    })
}
