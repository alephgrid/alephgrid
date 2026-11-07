//! Liveness, readiness and request correlation for operators (9.13, docs/OPERATIONS.md).
//!
//! * `GET /healthz`: the process is up and serving HTTP. No I/O; use it as a liveness probe.
//! * `GET /readyz`: the server can do its work: it is not shutting down, the data directory
//!   accepts writes and the control database answers. 503 when one of those fails. A released
//!   disk reserve (the disk filled up) is reported as `degraded` with 200, so administrators
//!   can still reach the server to free space.
//!
//! Neither needs a sign-in, so both say only which check failed: no paths, sizes or errors.

use std::sync::atomic::Ordering;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::state::Shared;

/// The header carrying a request's correlation id, both ways.
pub const REQUEST_ID: &str = "x-request-id";

pub async fn healthz() -> Response {
    Json(json!({ "status": "ok" })).into_response()
}

pub async fn readyz(State(state): State<Shared>) -> Response {
    let s = state.clone();
    let checks = tokio::task::spawn_blocking(move || readiness(&s)).await.unwrap_or_else(|_| Readiness::failed());
    let status = if !checks.ready() {
        "unavailable"
    } else if checks.disk_reserve == "released" {
        "degraded"
    } else {
        "ok"
    };
    let code = if checks.ready() { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    let body = json!({
        "status": status,
        "checks": {
            "accepting_work": ok(checks.accepting_work),
            "data_dir_writable": ok(checks.data_dir_writable),
            "control_db": ok(checks.control_db),
            "disk_reserve": checks.disk_reserve,
        },
    });
    let mut res = (code, Json(body)).into_response();
    res.headers_mut().insert(axum::http::header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn ok(b: bool) -> &'static str {
    if b {
        "ok"
    } else {
        "failed"
    }
}

pub struct Readiness {
    pub accepting_work: bool,
    pub data_dir_writable: bool,
    pub control_db: bool,
    /// `held`, `released` (the disk filled up, or had no room for it at start) or `off`.
    pub disk_reserve: &'static str,
}

impl Readiness {
    fn failed() -> Readiness {
        Readiness { accepting_work: false, data_dir_writable: false, control_db: false, disk_reserve: "off" }
    }

    pub fn ready(&self) -> bool {
        self.accepting_work && self.data_dir_writable && self.control_db
    }
}

/// Run the checks (blocking: it writes a small file and queries the control database).
pub fn readiness(state: &Shared) -> Readiness {
    let reserve = &state.controller.disk_reserve;
    Readiness {
        accepting_work: !state.shutting_down.load(Ordering::SeqCst),
        data_dir_writable: probe_write(&state.config.data_dir.join(".control")),
        control_db: state.auth.store.setting("default_space").is_ok_and(|s| s.is_some()),
        disk_reserve: match (reserve.bytes(), reserve.held()) {
            (0, _) => "off",
            (_, true) => "held",
            (_, false) => "released",
        },
    }
}

/// Create, write and remove a small file: what the server needs to record state. Each call
/// uses its own file, so probes running at the same time do not remove each other's.
pub fn probe_write(dir: &std::path::Path) -> bool {
    use std::io::Write;
    static PROBE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = PROBE.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!(".ready-{}-{n}", std::process::id()));
    let written = std::fs::File::create(&path).and_then(|mut f| f.write_all(b"ready\n"));
    let removed = std::fs::remove_file(&path);
    written.is_ok() && removed.is_ok()
}

/// Give every request a correlation id: the caller's `x-request-id` when it is short and
/// plain (a proxy's own id), else a new random one. It is passed on to the handler, named
/// in the request's log span and returned in the response.
pub async fn request_id(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)))
        .map(str::to_string)
        .unwrap_or_else(new_id);
    let value = HeaderValue::from_str(&id).expect("a request id is plain ASCII");
    req.headers_mut().insert(REQUEST_ID, value.clone());
    let mut res = next.run(req).await;
    res.headers_mut().insert(REQUEST_ID, value);
    res
}

fn new_id() -> String {
    let mut b = [0u8; 8];
    if getrandom::fill(&mut b).is_err() {
        return "unknown".into();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The log span of a request: its id, method and path. Never the query string (it can hold
/// an OIDC code or a file name), headers (cookies, tokens) or the body.
pub fn request_span(req: &Request) -> tracing::Span {
    let id = req.headers().get(REQUEST_ID).and_then(|v| v.to_str().ok()).unwrap_or("");
    tracing::info_span!("request", request_id = id, method = %req.method(), path = req.uri().path())
}
