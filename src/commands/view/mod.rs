use std::{
    io,
    path::{Path, PathBuf},
};

use serde::Serialize;
use tiny_http::{Header, Method, Request, Response, Server};

pub mod model;
mod sources;

pub const DEFAULT_PORT: u16 = 8787;

const INDEX_HTML: &str = include_str!("assets/index.html");
const APP_JS: &str = include_str!("assets/app.js");
const APP_CSS: &str = include_str!("assets/app.css");

/// Starts the local run-visualizer dashboard. Blocks serving requests until
/// the process is interrupted (Ctrl-C).
pub fn run(port: u16, results_dir: PathBuf) -> i32 {
    let address = format!("127.0.0.1:{port}");
    let server = match Server::http(&address) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("Error starting view server on {address}: {error}");
            return 1;
        }
    };

    println!("HayCut view running at http://{address} (Ctrl-C to stop)");

    for request in server.incoming_requests() {
        if let Err(error) = handle_request(request, &results_dir) {
            eprintln!("Error handling request: {error}");
        }
    }

    0
}

fn handle_request(request: Request, results_dir: &Path) -> io::Result<()> {
    let path = request.url().to_string();
    let method = request.method().clone();

    match (method, path.as_str()) {
        (Method::Get, "/") => {
            respond_bytes(request, INDEX_HTML.as_bytes(), "text/html; charset=utf-8")
        }
        (Method::Get, "/assets/app.js") => respond_bytes(
            request,
            APP_JS.as_bytes(),
            "application/javascript; charset=utf-8",
        ),
        (Method::Get, "/assets/app.css") => {
            respond_bytes(request, APP_CSS.as_bytes(), "text/css; charset=utf-8")
        }
        (Method::Get, "/api/runs") => respond_runs(request, results_dir),
        (Method::Get, other) if other.starts_with("/api/runs/") => {
            let id = other.trim_start_matches("/api/runs/").to_string();
            respond_run_detail(request, results_dir, &id)
        }
        _ => respond_status(request, 404, "not found"),
    }
}

fn respond_runs(request: Request, results_dir: &std::path::Path) -> io::Result<()> {
    match sources::list_runs(results_dir) {
        Ok(runs) => respond_json(request, &runs),
        Err(error) => respond_status(request, 500, &error.to_string()),
    }
}

fn respond_run_detail(request: Request, results_dir: &std::path::Path, id: &str) -> io::Result<()> {
    match sources::load_run(results_dir, id) {
        Ok(detail) => respond_json(request, &detail),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            respond_status(request, 404, &error.to_string())
        }
        Err(error) => respond_status(request, 500, &error.to_string()),
    }
}

fn respond_bytes(request: Request, body: &[u8], content_type: &str) -> io::Result<()> {
    let header = Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes())
        .expect("static content-type header is valid ASCII");
    let response = Response::from_data(body.to_vec()).with_header(header);
    request.respond(response)
}

fn respond_json<T: Serialize>(request: Request, value: &T) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    respond_bytes(request, &body, "application/json")
}

fn respond_status(request: Request, status: u16, message: &str) -> io::Result<()> {
    let header = Header::from_bytes(&b"Content-Type"[..], b"text/plain; charset=utf-8")
        .expect("static content-type header is valid ASCII");
    let response = Response::from_data(message.as_bytes().to_vec())
        .with_status_code(status)
        .with_header(header);
    request.respond(response)
}
