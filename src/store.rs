use std::{fs, io, path::Path};

use rusqlite::{Connection, OptionalExtension, params};

pub const RUN_STORE_PATH: &str = ".haycut/haycut.sqlite3";
pub const SCHEMA_VERSION: i32 = 3;

#[derive(Debug)]
pub struct NewRun<'a> {
    pub id: &'a str,
    pub command: &'a str,
    pub args_json: &'a str,
    pub cwd: &'a str,
    pub exit_code: Option<i32>,
    pub duration_ms: i64,
    pub stdout_bytes: i64,
    pub stderr_bytes: i64,
    pub raw_tokens: i64,
    pub raw_stdout_tokens: i64,
    pub raw_stderr_tokens: i64,
    pub packet_tokens: i64,
    pub created_at: &'a str,
    pub stdout_path: &'a str,
    pub stderr_path: &'a str,
    pub compact_text_path: Option<&'a str>,
    pub compact_json: &'a str,
    pub evidence_json: &'a str,
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
    pub args_json: String,
    pub cwd: String,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub stdout_bytes: Option<i64>,
    pub stderr_bytes: Option<i64>,
    pub raw_tokens: Option<i64>,
    pub raw_stdout_tokens: Option<i64>,
    pub raw_stderr_tokens: Option<i64>,
    pub packet_tokens: Option<i64>,
    pub created_at: String,
    pub stdout_path: String,
    pub stderr_path: String,
    pub compact_text_path: Option<String>,
    pub compact_json: String,
    pub evidence_json: String,
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

#[derive(Debug, PartialEq, Eq)]
pub struct StoredTask {
    pub id: String,
    pub title: String,
    pub status: String,
    pub task_json: String,
    pub updated_at: String,
}

#[derive(Debug)]
pub struct NewAgentTrace<'a> {
    pub id: &'a str,
    pub task_id: &'a str,
    pub step_index: i64,
    pub prompt: &'a str,
    pub response: &'a str,
    pub action_json: &'a str,
    pub observation: &'a str,
    pub estimated_input_tokens: i64,
    pub estimated_output_tokens: i64,
    pub reported_input_tokens: Option<i64>,
    pub reported_output_tokens: Option<i64>,
    pub created_at: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StoredAgentTrace {
    pub id: String,
    pub task_id: String,
    pub step_index: i64,
    pub prompt: String,
    pub response: String,
    pub action_json: String,
    pub observation: String,
    pub estimated_input_tokens: i64,
    pub estimated_output_tokens: i64,
    pub reported_input_tokens: Option<i64>,
    pub reported_output_tokens: Option<i64>,
    pub created_at: String,
}

impl StoredRun {
    #[allow(dead_code)]
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
                id, command, args_json, cwd, exit_code, duration_ms,
                stdout_bytes, stderr_bytes, raw_tokens, raw_stdout_tokens, raw_stderr_tokens,
                packet_tokens, created_at, stdout_path, stderr_path, compact_text_path,
                compact_json, evidence_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                run.id,
                run.command,
                run.args_json,
                run.cwd,
                run.exit_code,
                run.duration_ms,
                run.stdout_bytes,
                run.stderr_bytes,
                run.raw_tokens,
                run.raw_stdout_tokens,
                run.raw_stderr_tokens,
                run.packet_tokens,
                run.created_at,
                run.stdout_path,
                run.stderr_path,
                run.compact_text_path,
                run.compact_json,
                run.evidence_json,
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
        "SELECT id, command, args_json, cwd, exit_code, duration_ms,
            stdout_bytes, stderr_bytes, raw_tokens, raw_stdout_tokens, raw_stderr_tokens,
            packet_tokens, created_at, stdout_path, stderr_path, compact_text_path,
            compact_json, evidence_json
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
                args_json: row.get(2)?,
                cwd: row.get(3)?,
                exit_code: row.get(4)?,
                duration_ms: row.get(5)?,
                stdout_bytes: row.get(6)?,
                stderr_bytes: row.get(7)?,
                raw_tokens: row.get(8)?,
                raw_stdout_tokens: row.get(9)?,
                raw_stderr_tokens: row.get(10)?,
                packet_tokens: row.get(11)?,
                created_at: row.get(12)?,
                stdout_path: row.get(13)?,
                stderr_path: row.get(14)?,
                compact_text_path: row.get(15)?,
                compact_json: row.get(16)?,
                evidence_json: row.get(17)?,
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

pub fn upsert_task(db_path: &Path, task: &StoredTask, current: bool) -> io::Result<()> {
    let conn = open_migrated(db_path)?;

    conn.execute(
        "INSERT OR REPLACE INTO tasks (id, title, status, task_json, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            task.id,
            task.title,
            task.status,
            task.task_json,
            task.updated_at
        ],
    )
    .map_err(io::Error::other)?;

    if current {
        set_setting(&conn, "current_task_id", &task.id)?;
    }

    Ok(())
}

pub fn current_task(db_path: &Path) -> io::Result<StoredTask> {
    let conn = open_migrated(db_path)?;
    let task_id = setting(&conn, "current_task_id")?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no current task found"))?;

    load_task_by_id(&conn, &task_id)
}

pub fn list_tasks(db_path: &Path) -> io::Result<Vec<StoredTask>> {
    let conn = open_migrated(db_path)?;
    let mut statement = conn
        .prepare(
            "SELECT id, title, status, task_json, updated_at
            FROM tasks
            ORDER BY updated_at DESC, id DESC",
        )
        .map_err(io::Error::other)?;

    let rows = statement
        .query_map([], stored_task_from_row)
        .map_err(io::Error::other)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)
}

pub fn close_current_task(db_path: &Path, task_json: &str, closed_at: &str) -> io::Result<String> {
    let conn = open_migrated(db_path)?;
    let task_id = setting(&conn, "current_task_id")?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no current task found"))?;

    conn.execute(
        "UPDATE tasks
        SET status = 'closed', task_json = ?2, updated_at = ?3
        WHERE id = ?1",
        params![task_id, task_json, closed_at],
    )
    .map_err(io::Error::other)?;
    delete_setting(&conn, "current_task_id")?;

    Ok(task_id)
}

pub fn insert_agent_trace(db_path: &Path, trace: &NewAgentTrace<'_>) -> io::Result<()> {
    let conn = open_migrated(db_path)?;

    conn.execute(
        "INSERT INTO agent_traces (
            id, task_id, step_index, prompt, response, action_json, observation,
            estimated_input_tokens, estimated_output_tokens,
            reported_input_tokens, reported_output_tokens, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            trace.id,
            trace.task_id,
            trace.step_index,
            trace.prompt,
            trace.response,
            trace.action_json,
            trace.observation,
            trace.estimated_input_tokens,
            trace.estimated_output_tokens,
            trace.reported_input_tokens,
            trace.reported_output_tokens,
            trace.created_at,
        ],
    )
    .map_err(io::Error::other)?;

    Ok(())
}

pub fn agent_traces_for_task(db_path: &Path, task_id: &str) -> io::Result<Vec<StoredAgentTrace>> {
    let conn = open_migrated(db_path)?;
    let mut statement = conn
        .prepare(
            "SELECT id, task_id, step_index, prompt, response, action_json, observation,
                estimated_input_tokens, estimated_output_tokens,
                reported_input_tokens, reported_output_tokens, created_at
            FROM agent_traces
            WHERE task_id = ?1
            ORDER BY step_index ASC, created_at ASC, id ASC",
        )
        .map_err(io::Error::other)?;

    let rows = statement
        .query_map(params![task_id], |row| {
            Ok(StoredAgentTrace {
                id: row.get(0)?,
                task_id: row.get(1)?,
                step_index: row.get(2)?,
                prompt: row.get(3)?,
                response: row.get(4)?,
                action_json: row.get(5)?,
                observation: row.get(6)?,
                estimated_input_tokens: row.get(7)?,
                estimated_output_tokens: row.get(8)?,
                reported_input_tokens: row.get(9)?,
                reported_output_tokens: row.get(10)?,
                created_at: row.get(11)?,
            })
        })
        .map_err(io::Error::other)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)
}

fn load_task_by_id(conn: &Connection, task_id: &str) -> io::Result<StoredTask> {
    conn.query_row(
        "SELECT id, title, status, task_json, updated_at
        FROM tasks
        WHERE id = ?1",
        params![task_id],
        stored_task_from_row,
    )
    .optional()
    .map_err(io::Error::other)?
    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("task {task_id} not found")))
}

fn stored_task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredTask> {
    Ok(StoredTask {
        id: row.get(0)?,
        title: row.get(1)?,
        status: row.get(2)?,
        task_json: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn setting(conn: &Connection, key: &str) -> io::Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
    .map_err(io::Error::other)
}

fn set_setting(conn: &Connection, key: &str, value: &str) -> io::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .map_err(io::Error::other)?;

    Ok(())
}

fn delete_setting(conn: &Connection, key: &str) -> io::Result<()> {
    conn.execute("DELETE FROM settings WHERE key = ?1", params![key])
        .map_err(io::Error::other)?;

    Ok(())
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
            args_json text not null,
            cwd text not null,
            exit_code integer,
            duration_ms integer,
            stdout_bytes integer,
            stderr_bytes integer,
            raw_tokens integer,
            raw_stdout_tokens integer,
            raw_stderr_tokens integer,
            packet_tokens integer,
            created_at text not null,
            stdout_path text not null,
            stderr_path text not null,
            compact_text_path text,
            compact_json text not null,
            evidence_json text not null
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

        CREATE INDEX IF NOT EXISTS idx_files_estimated_tokens ON files(estimated_tokens);

        CREATE TABLE IF NOT EXISTS tasks(
            id text primary key,
            title text not null,
            status text not null,
            task_json text not null,
            updated_at text not null
        );

        CREATE TABLE IF NOT EXISTS settings(
            key text primary key,
            value text not null
        );

        CREATE INDEX IF NOT EXISTS idx_tasks_updated_at ON tasks(updated_at);",
    )
    .map_err(io::Error::other)?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_traces(
            id text primary key,
            task_id text not null,
            step_index integer not null,
            prompt text not null,
            response text not null,
            action_json text not null,
            observation text not null,
            estimated_input_tokens integer not null,
            estimated_output_tokens integer not null,
            reported_input_tokens integer,
            reported_output_tokens integer,
            created_at text not null,
            foreign key(task_id) references tasks(id) on delete cascade
        );

        CREATE INDEX IF NOT EXISTS idx_agent_traces_task_step ON agent_traces(task_id, step_index);",
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
        let artifacts = vec![NewArtifact {
            id: "2026-07-07T153000Z-a1b2c3:stdout".to_string(),
            kind: "stdout",
            path: ".haycut/runs/2026-07-07T153000Z-a1b2c3/stdout.txt".to_string(),
            estimated_tokens: Some(80),
        }];
        let run = new_run(
            "2026-07-07T153000Z-a1b2c3",
            "cargo test",
            Some(101),
            100,
            12,
            "2026-07-07T15:30:00+00:00",
            artifacts,
        );

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
            &new_run(
                "older",
                "older command",
                Some(0),
                1,
                1,
                "2026-07-07T15:29:00+00:00",
                Vec::new(),
            ),
        )
        .expect("older run should insert");
        insert_run(
            &db_path,
            &new_run(
                "newer",
                "newer command",
                Some(0),
                1,
                1,
                "2026-07-07T15:30:00+00:00",
                Vec::new(),
            ),
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
            &new_run(
                "oldest",
                "oldest command",
                Some(0),
                100,
                50,
                "2026-07-07T15:28:00+00:00",
                Vec::new(),
            ),
        )
        .expect("oldest run should insert");
        insert_run(
            &db_path,
            &new_run(
                "middle",
                "middle command",
                Some(1),
                100,
                25,
                "2026-07-07T15:29:00+00:00",
                Vec::new(),
            ),
        )
        .expect("middle run should insert");
        insert_run(
            &db_path,
            &new_run(
                "newest",
                "newest command",
                Some(2),
                100,
                10,
                "2026-07-07T15:30:00+00:00",
                Vec::new(),
            ),
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

    #[test]
    fn inserts_and_lists_agent_traces_for_task() {
        let db_path = temp_db_path("agent-traces");
        let task = StoredTask {
            id: "task-1".to_string(),
            title: "Fix failing config test".to_string(),
            status: "open".to_string(),
            task_json: "{}".to_string(),
            updated_at: "2026-07-08T15:00:00+00:00".to_string(),
        };
        upsert_task(&db_path, &task, true).expect("task should store");

        insert_agent_trace(
            &db_path,
            &NewAgentTrace {
                id: "trace-1",
                task_id: "task-1",
                step_index: 1,
                prompt: "TASK\nGoal: Fix failing config test",
                response: r#"{"action":"read_symbol"}"#,
                action_json: r#"{"action":"read_symbol"}"#,
                observation: "read_symbol found create_default_config_at",
                estimated_input_tokens: 100,
                estimated_output_tokens: 20,
                reported_input_tokens: Some(90),
                reported_output_tokens: Some(12),
                created_at: "2026-07-08T15:01:00+00:00",
            },
        )
        .expect("trace should store");

        let traces = agent_traces_for_task(&db_path, "task-1").expect("trace should load");

        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].step_index, 1);
        assert_eq!(traces[0].estimated_input_tokens, 100);
        assert_eq!(traces[0].reported_output_tokens, Some(12));
        assert!(traces[0].observation.contains("create_default_config_at"));

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

    fn new_run<'a>(
        id: &'a str,
        command: &'a str,
        exit_code: Option<i32>,
        raw_tokens: i64,
        packet_tokens: i64,
        created_at: &'a str,
        artifacts: Vec<NewArtifact<'a>>,
    ) -> NewRun<'a> {
        NewRun {
            id,
            command,
            args_json: "[]",
            cwd: "/tmp/project",
            exit_code,
            duration_ms: 1,
            stdout_bytes: 0,
            stderr_bytes: 0,
            raw_tokens,
            raw_stdout_tokens: 0,
            raw_stderr_tokens: raw_tokens,
            packet_tokens,
            created_at,
            stdout_path: "stdout.txt",
            stderr_path: "stderr.txt",
            compact_text_path: None,
            compact_json: "{\"compactor\":\"native\",\"rtk_version\":null,\"command\":\"test\",\"exit_code\":0,\"duration_ms\":1,\"failed\":false,\"stdout_artifact\":\"stdout.txt\",\"stderr_artifact\":\"stderr.txt\",\"compact_artifact\":null,\"raw_stdout_tokens\":0,\"raw_stderr_tokens\":0,\"raw_tokens\":0,\"packet_tokens\":0,\"preserved_items\":[],\"omitted_items\":[],\"notes\":[]}",
            evidence_json: "{\"schema_version\":1,\"run_id\":\"test\",\"outcome\":{\"exit_code\":0,\"status\":\"success\"},\"diagnostics\":[],\"file_refs\":[],\"context_items\":[],\"token_summary\":{\"raw_tokens\":0,\"packet_tokens\":0,\"saved_tokens\":0,\"reduction_percent\":0.0}}",
            artifacts,
        }
    }
}
