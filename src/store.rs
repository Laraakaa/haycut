use std::{fs, io, path::Path};

use rusqlite::{Connection, OptionalExtension, params};

pub const RUN_STORE_PATH: &str = ".haycut/haycut.sqlite3";
pub const SCHEMA_VERSION: i32 = 1;

#[derive(Debug)]
pub struct NewRun<'a> {
    pub id: &'a str,
    pub command: &'a str,
    pub cwd: &'a str,
    pub exit_code: Option<i32>,
    pub duration_ms: i64,
    pub raw_tokens: i64,
    pub packet_tokens: i64,
    pub created_at: &'a str,
    pub artifacts: Vec<NewArtifact<'a>>,
}

#[derive(Debug)]
pub struct NewArtifact<'a> {
    pub id: String,
    pub kind: &'a str,
    pub path: String,
    pub estimated_tokens: Option<i64>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StoredRun {
    pub id: String,
    pub command: String,
    pub cwd: String,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub raw_tokens: Option<i64>,
    pub packet_tokens: Option<i64>,
    pub created_at: String,
    pub artifacts: Vec<StoredArtifact>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StoredArtifact {
    pub kind: String,
    pub path: String,
    pub estimated_tokens: Option<i64>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RunSummary {
    pub id: String,
    pub command: String,
    pub exit_code: Option<i32>,
    pub raw_tokens: Option<i64>,
    pub packet_tokens: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FileInventoryEntry {
    pub path: String,
    pub language: Option<String>,
    pub byte_size: i64,
    pub line_count: i64,
    pub estimated_tokens: i64,
    pub modified_at: Option<String>,
    pub content_hash: String,
}

impl StoredRun {
    pub fn artifact_path(&self, kind: &str) -> io::Result<&str> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.kind == kind)
            .map(|artifact| artifact.path.as_str())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("run {} has no {kind} artifact", self.id),
                )
            })
    }
}

pub fn insert_run(db_path: &Path, run: &NewRun<'_>) -> io::Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut conn = open_migrated(db_path)?;
    let transaction = conn.transaction().map_err(io::Error::other)?;

    transaction
        .execute(
            "INSERT OR REPLACE INTO runs (
                id, command, cwd, exit_code, duration_ms, raw_tokens, packet_tokens, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run.id,
                run.command,
                run.cwd,
                run.exit_code,
                run.duration_ms,
                run.raw_tokens,
                run.packet_tokens,
                run.created_at,
            ],
        )
        .map_err(io::Error::other)?;

    transaction
        .execute("DELETE FROM artifacts WHERE run_id = ?1", params![run.id])
        .map_err(io::Error::other)?;

    for artifact in &run.artifacts {
        transaction
            .execute(
                "INSERT INTO artifacts (id, run_id, kind, path, estimated_tokens)
                VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    artifact.id,
                    run.id,
                    artifact.kind,
                    artifact.path,
                    artifact.estimated_tokens,
                ],
            )
            .map_err(io::Error::other)?;
    }

    transaction.commit().map_err(io::Error::other)
}

pub fn latest_run(db_path: &Path) -> io::Result<StoredRun> {
    load_latest_run_where(db_path, "1 = 1", "no runs found in SQLite")
}

pub fn latest_failed_run(db_path: &Path) -> io::Result<StoredRun> {
    load_latest_run_where(
        db_path,
        "exit_code IS NOT NULL AND exit_code != 0",
        "no failed runs found in SQLite",
    )
}

fn load_latest_run_where(
    db_path: &Path,
    where_clause: &str,
    empty_message: &str,
) -> io::Result<StoredRun> {
    let conn = open_migrated(db_path)?;
    let query = format!(
        "SELECT id, command, cwd, exit_code, duration_ms, raw_tokens, packet_tokens, created_at
        FROM runs
        WHERE {where_clause}
        ORDER BY created_at DESC, id DESC
        LIMIT 1"
    );
    let mut statement = conn.prepare(&query).map_err(io::Error::other)?;

    let run = statement
        .query_row([], |row| {
            Ok(StoredRun {
                id: row.get(0)?,
                command: row.get(1)?,
                cwd: row.get(2)?,
                exit_code: row.get(3)?,
                duration_ms: row.get(4)?,
                raw_tokens: row.get(5)?,
                packet_tokens: row.get(6)?,
                created_at: row.get(7)?,
                artifacts: Vec::new(),
            })
        })
        .optional()
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, empty_message))?;

    Ok(StoredRun {
        artifacts: load_artifacts(&conn, &run.id)?,
        ..run
    })
}

pub fn recent_runs(db_path: &Path, limit: usize) -> io::Result<Vec<RunSummary>> {
    let conn = open_migrated(db_path)?;
    let mut statement = conn
        .prepare(
            "SELECT id, command, exit_code, raw_tokens, packet_tokens, created_at
            FROM runs
            ORDER BY created_at DESC, id DESC
            LIMIT ?1",
        )
        .map_err(io::Error::other)?;

    let rows = statement
        .query_map(params![limit as i64], |row| {
            Ok(RunSummary {
                id: row.get(0)?,
                command: row.get(1)?,
                exit_code: row.get(2)?,
                raw_tokens: row.get(3)?,
                packet_tokens: row.get(4)?,
                created_at: row.get(5)?,
            })
        })
        .map_err(io::Error::other)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)
}

pub fn replace_file_inventory(db_path: &Path, files: &[FileInventoryEntry]) -> io::Result<()> {
    let mut conn = open_migrated(db_path)?;
    let transaction = conn.transaction().map_err(io::Error::other)?;

    transaction
        .execute("DELETE FROM files", [])
        .map_err(io::Error::other)?;

    for file in files {
        transaction
            .execute(
                "INSERT INTO files (
                    path, language, byte_size, line_count, estimated_tokens, modified_at, content_hash
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    file.path,
                    file.language,
                    file.byte_size,
                    file.line_count,
                    file.estimated_tokens,
                    file.modified_at,
                    file.content_hash,
                ],
            )
            .map_err(io::Error::other)?;
    }

    transaction.commit().map_err(io::Error::other)
}

pub fn largest_files(db_path: &Path, limit: usize) -> io::Result<Vec<FileInventoryEntry>> {
    let conn = open_migrated(db_path)?;
    let mut statement = conn
        .prepare(
            "SELECT path, language, byte_size, line_count, estimated_tokens, modified_at, content_hash
            FROM files
            ORDER BY estimated_tokens DESC, byte_size DESC, path ASC
            LIMIT ?1",
        )
        .map_err(io::Error::other)?;

    let rows = statement
        .query_map(params![limit as i64], |row| {
            Ok(FileInventoryEntry {
                path: row.get(0)?,
                language: row.get(1)?,
                byte_size: row.get(2)?,
                line_count: row.get(3)?,
                estimated_tokens: row.get(4)?,
                modified_at: row.get(5)?,
                content_hash: row.get(6)?,
            })
        })
        .map_err(io::Error::other)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)
}

fn load_artifacts(conn: &Connection, run_id: &str) -> io::Result<Vec<StoredArtifact>> {
    let mut statement = conn
        .prepare(
            "SELECT kind, path, estimated_tokens
            FROM artifacts
            WHERE run_id = ?1
            ORDER BY kind",
        )
        .map_err(io::Error::other)?;

    let rows = statement
        .query_map(params![run_id], |row| {
            Ok(StoredArtifact {
                kind: row.get(0)?,
                path: row.get(1)?,
                estimated_tokens: row.get(2)?,
            })
        })
        .map_err(io::Error::other)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)
}

fn open_migrated(db_path: &Path) -> io::Result<Connection> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(db_path).map_err(io::Error::other)?;
    migrate(&conn)?;

    Ok(conn)
}

fn migrate(conn: &Connection) -> io::Result<()> {
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(io::Error::other)?;

    if version > SCHEMA_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "run store schema version {version} is newer than supported version {SCHEMA_VERSION}"
            ),
        ));
    }

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS runs(
            id text primary key,
            command text not null,
            cwd text not null,
            exit_code integer,
            duration_ms integer,
            raw_tokens integer,
            packet_tokens integer,
            created_at text not null
        );

        CREATE TABLE IF NOT EXISTS artifacts(
            id text primary key,
            run_id text not null,
            kind text not null,
            path text not null,
            estimated_tokens integer,
            foreign key(run_id) references runs(id) on delete cascade
        );

        CREATE INDEX IF NOT EXISTS idx_runs_created_at ON runs(created_at);
        CREATE INDEX IF NOT EXISTS idx_artifacts_run_id ON artifacts(run_id);

        CREATE TABLE IF NOT EXISTS files(
            path text primary key,
            language text,
            byte_size integer not null,
            line_count integer not null,
            estimated_tokens integer not null,
            modified_at text,
            content_hash text not null
        );

        CREATE INDEX IF NOT EXISTS idx_files_estimated_tokens ON files(estimated_tokens);",
    )
    .map_err(io::Error::other)?;

    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn insert_run_initializes_schema_and_persists_artifacts() {
        let db_path = temp_db_path("insert-run");
        let run = NewRun {
            id: "2026-07-07T153000Z-a1b2c3",
            command: "cargo test",
            cwd: "/tmp/project",
            exit_code: Some(101),
            duration_ms: 42,
            raw_tokens: 100,
            packet_tokens: 12,
            created_at: "2026-07-07T15:30:00+00:00",
            artifacts: vec![NewArtifact {
                id: "2026-07-07T153000Z-a1b2c3:stdout".to_string(),
                kind: "stdout",
                path: ".haycut/runs/2026-07-07T153000Z-a1b2c3/stdout.txt".to_string(),
                estimated_tokens: Some(80),
            }],
        };

        insert_run(&db_path, &run).expect("run should insert");

        let conn = Connection::open(&db_path).expect("store should open");
        let schema_version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version should read");
        let stored = latest_run(&db_path).expect("latest run should load");

        assert_eq!(schema_version, SCHEMA_VERSION);
        assert_eq!(stored.id, run.id);
        assert_eq!(stored.command, run.command);
        assert_eq!(stored.raw_tokens, Some(100));
        assert_eq!(stored.packet_tokens, Some(12));
        assert_eq!(
            stored.artifact_path("stdout").expect("stdout should exist"),
            run.artifacts[0].path
        );

        fs::remove_file(db_path).expect("test db should be removed");
    }

    #[test]
    fn latest_run_uses_created_at_ordering() {
        let db_path = temp_db_path("latest-run");
        insert_run(
            &db_path,
            &NewRun {
                id: "older",
                command: "older command",
                cwd: "/tmp/project",
                exit_code: Some(0),
                duration_ms: 1,
                raw_tokens: 1,
                packet_tokens: 1,
                created_at: "2026-07-07T15:29:00+00:00",
                artifacts: Vec::new(),
            },
        )
        .expect("older run should insert");
        insert_run(
            &db_path,
            &NewRun {
                id: "newer",
                command: "newer command",
                cwd: "/tmp/project",
                exit_code: Some(0),
                duration_ms: 1,
                raw_tokens: 1,
                packet_tokens: 1,
                created_at: "2026-07-07T15:30:00+00:00",
                artifacts: Vec::new(),
            },
        )
        .expect("newer run should insert");

        let stored = latest_run(&db_path).expect("latest run should load");

        assert_eq!(stored.id, "newer");

        fs::remove_file(db_path).expect("test db should be removed");
    }

    #[test]
    fn recent_runs_respects_limit_and_created_at_ordering() {
        let db_path = temp_db_path("recent-runs");
        insert_run(
            &db_path,
            &NewRun {
                id: "oldest",
                command: "oldest command",
                cwd: "/tmp/project",
                exit_code: Some(0),
                duration_ms: 1,
                raw_tokens: 100,
                packet_tokens: 50,
                created_at: "2026-07-07T15:28:00+00:00",
                artifacts: Vec::new(),
            },
        )
        .expect("oldest run should insert");
        insert_run(
            &db_path,
            &NewRun {
                id: "middle",
                command: "middle command",
                cwd: "/tmp/project",
                exit_code: Some(1),
                duration_ms: 1,
                raw_tokens: 100,
                packet_tokens: 25,
                created_at: "2026-07-07T15:29:00+00:00",
                artifacts: Vec::new(),
            },
        )
        .expect("middle run should insert");
        insert_run(
            &db_path,
            &NewRun {
                id: "newest",
                command: "newest command",
                cwd: "/tmp/project",
                exit_code: Some(2),
                duration_ms: 1,
                raw_tokens: 100,
                packet_tokens: 10,
                created_at: "2026-07-07T15:30:00+00:00",
                artifacts: Vec::new(),
            },
        )
        .expect("newest run should insert");

        let runs = recent_runs(&db_path, 2).expect("recent runs should load");

        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, "newest");
        assert_eq!(runs[1].id, "middle");
        assert_eq!(runs[0].raw_tokens, Some(100));
        assert_eq!(runs[0].packet_tokens, Some(10));

        fs::remove_file(db_path).expect("test db should be removed");
    }

    #[test]
    fn replaces_and_queries_largest_files() {
        let db_path = temp_db_path("files");
        replace_file_inventory(
            &db_path,
            &[
                FileInventoryEntry {
                    path: "small.rs".to_string(),
                    language: Some("Rust".to_string()),
                    byte_size: 12,
                    line_count: 1,
                    estimated_tokens: 3,
                    modified_at: Some("2026-07-07T15:29:00Z".to_string()),
                    content_hash: "a".to_string(),
                },
                FileInventoryEntry {
                    path: "large.ts".to_string(),
                    language: Some("TypeScript".to_string()),
                    byte_size: 400,
                    line_count: 20,
                    estimated_tokens: 100,
                    modified_at: Some("2026-07-07T15:30:00Z".to_string()),
                    content_hash: "b".to_string(),
                },
            ],
        )
        .expect("file inventory should store");

        let files = largest_files(&db_path, 1).expect("largest files should load");

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "large.ts");
        assert_eq!(files[0].language.as_deref(), Some("TypeScript"));
        assert_eq!(files[0].estimated_tokens, 100);

        replace_file_inventory(&db_path, &[]).expect("file inventory should clear");
        let files = largest_files(&db_path, 10).expect("largest files should load after clear");
        assert!(files.is_empty());

        fs::remove_file(db_path).expect("test db should be removed");
    }

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        env::temp_dir().join(format!(
            "haycut-store-{label}-{}-{}.sqlite3",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos()
        ))
    }
}
