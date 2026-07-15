use std::{fs, io, path::Path};

use crate::{
    commands::view::model::{
        EvalReportFile, RunDetail, RunSummaryView, task_to_detail, task_to_summary,
    },
    store::{self, RUN_STORE_PATH},
};

const EVAL_PREFIX: &str = "eval:";
const TASK_PREFIX: &str = "task:";

/// Lists every viewable run: eval results (newest first) followed by tasks
/// recorded in the local SQLite store (open tasks first).
pub fn list_runs(results_dir: &Path) -> io::Result<Vec<RunSummaryView>> {
    let mut runs = list_eval_runs(results_dir)?;
    runs.extend(list_task_runs(Path::new(RUN_STORE_PATH))?);
    Ok(runs)
}

pub fn load_run(results_dir: &Path, id: &str) -> io::Result<RunDetail> {
    if let Some(dir_name) = id.strip_prefix(EVAL_PREFIX) {
        return load_eval_run(results_dir, dir_name);
    }
    if let Some(task_id) = id.strip_prefix(TASK_PREFIX) {
        return load_task_run(Path::new(RUN_STORE_PATH), task_id);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("unrecognized run id: {id}"),
    ))
}

fn list_eval_runs(results_dir: &Path) -> io::Result<Vec<RunSummaryView>> {
    if !results_dir.exists() {
        return Ok(Vec::new());
    }

    let mut dir_names: Vec<String> = fs::read_dir(results_dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join("report.json").exists())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    // Result directories are timestamp-prefixed, so lexical order is
    // chronological; newest first.
    dir_names.sort_unstable_by(|a, b| b.cmp(a));

    let mut runs = Vec::with_capacity(dir_names.len());
    for dir_name in dir_names {
        let report_path = results_dir.join(&dir_name).join("report.json");
        match read_eval_report(&report_path) {
            Ok(report) => runs.push(report.to_summary(format!("{EVAL_PREFIX}{dir_name}"))),
            Err(error) => {
                eprintln!("warning: skipping {}: {error}", report_path.display());
            }
        }
    }
    Ok(runs)
}

fn load_eval_run(results_dir: &Path, dir_name: &str) -> io::Result<RunDetail> {
    let report_path = results_dir.join(dir_name).join("report.json");
    let report = read_eval_report(&report_path)?;
    Ok(report.into_detail(format!("{EVAL_PREFIX}{dir_name}")))
}

fn read_eval_report(path: &Path) -> io::Result<EvalReportFile> {
    let contents = fs::read_to_string(path)?;
    serde_json::from_str(&contents).map_err(io::Error::other)
}

fn list_task_runs(db_path: &Path) -> io::Result<Vec<RunSummaryView>> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let stored_tasks = store::list_tasks(db_path)?;
    let mut runs = Vec::with_capacity(stored_tasks.len());
    for stored in stored_tasks {
        let task = match serde_json::from_str(&stored.task_json) {
            Ok(task) => task,
            Err(error) => {
                eprintln!("warning: skipping task {}: {error}", stored.id);
                continue;
            }
        };
        runs.push(task_to_summary(
            format!("{TASK_PREFIX}{}", stored.id),
            &stored.status,
            &task,
        ));
    }
    Ok(runs)
}

fn load_task_run(db_path: &Path, task_id: &str) -> io::Result<RunDetail> {
    let stored_tasks = store::list_tasks(db_path)?;
    let stored = stored_tasks
        .into_iter()
        .find(|task| task.id == task_id)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("task {task_id} not found"))
        })?;

    let task = serde_json::from_str(&stored.task_json).map_err(io::Error::other)?;
    let traces = store::agent_traces_for_task(db_path, task_id)?;
    let manifests = store::request_manifests_for_task(db_path, task_id)?;
    Ok(task_to_detail(
        format!("{TASK_PREFIX}{task_id}"),
        &stored.status,
        &task,
        &traces,
        manifests,
    ))
}
