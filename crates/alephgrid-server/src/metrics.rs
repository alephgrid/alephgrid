//! Lifecycle metrics in the Prometheus text format (9.13, docs/OPERATIONS.md#metrics).
//!
//! `GET /metrics` answers instance admins (every caller in `--auth local`, which serves only
//! loopback) and, when `--metrics-token` is set, a scraper sending
//! `Authorization: Bearer <token>`. Everyone else gets 401 or 403.
//!
//! Label values are `&'static str`: they can only come from the code (allocation states,
//! failure kinds, job classes and lanes), never from a request, so a series can never carry
//! a workbook id or name, a user, SQL text or a secret, and the number of series is fixed.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::runtime::{AllocationState, FailureKind};
use crate::sched::{Class, Lane};
use crate::state::Shared;

type Labels = Vec<(&'static str, &'static str)>;

/// Upper bounds (seconds) of the duration histograms' buckets.
pub const BUCKETS: [f64; 16] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0, 900.0, 3600.0];

#[derive(Clone, Default)]
struct Histogram {
    /// Observations per bucket (not cumulative); the last one is `+Inf`.
    buckets: [u64; BUCKETS.len() + 1],
    sum: f64,
    count: u64,
}

/// Counters and histograms recorded as things happen. Gauges are read when scraped.
#[derive(Default)]
pub struct Metrics {
    counters: Mutex<BTreeMap<(&'static str, Labels), u64>>,
    histograms: Mutex<BTreeMap<(&'static str, Labels), Histogram>>,
}

/// `HELP` text of every recorded family.
const HELP: &[(&str, &str, &str)] = &[
    ("alephgrid_allocation_starts_total", "counter", "Workbook allocation starts begun."),
    ("alephgrid_allocation_start_failures_total", "counter", "Allocation starts that did not reach ready, by reason."),
    ("alephgrid_allocation_stops_total", "counter", "Allocations stopped, by reason (evicted: idle or closed to make room)."),
    ("alephgrid_allocations_lost_total", "counter", "Allocations of a previous process found live at start and marked lost."),
    ("alephgrid_allocation_start_seconds", "histogram", "Time from an allocation's request to ready."),
    ("alephgrid_job_wait_seconds", "histogram", "Time a job waited in its workbook's queue before it ran."),
    ("alephgrid_job_duration_seconds", "histogram", "Time a job ran (queries, imports, edits, fetches)."),
    ("alephgrid_jobs_total", "counter", "Jobs ended, by class and outcome (rejected: the queue was full)."),
    ("alephgrid_websocket_connections_total", "counter", "WebSocket sessions opened."),
    ("alephgrid_imports_total", "counter", "File imports, by result."),
    ("alephgrid_import_bytes_total", "counter", "Bytes of files imported successfully."),
    ("alephgrid_disk_reserve_releases_total", "counter", "Times the disk reserve was given back because the disk filled up."),
];

pub fn failure_label(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Incompatible => "incompatible",
        FailureKind::Capacity => "capacity",
        FailureKind::Quota => "quota",
        FailureKind::OwnedElsewhere => "owned_elsewhere",
        FailureKind::StartupFailed => "startup_failed",
        FailureKind::Cancelled => "cancelled",
        FailureKind::Lost => "lost",
    }
}

pub fn class_label(class: Class) -> &'static str {
    match class {
        Class::Interactive => "interactive",
        Class::Bulk => "bulk",
    }
}

pub fn lane_label(lane: Lane) -> &'static str {
    match lane {
        Lane::Read => "read",
        Lane::Write => "write",
    }
}

pub fn state_label(state: AllocationState) -> &'static str {
    match state {
        AllocationState::Requested => "requested",
        AllocationState::Starting => "starting",
        AllocationState::Running => "running",
        AllocationState::Ready => "ready",
        AllocationState::Draining => "draining",
        AllocationState::Stopped => "stopped",
        AllocationState::Failed => "failed",
    }
}

/// The live allocation states, in lifecycle order.
pub const LIVE_STATES: [AllocationState; 5] =
    [AllocationState::Requested, AllocationState::Starting, AllocationState::Running, AllocationState::Ready, AllocationState::Draining];

const FAILURES: [FailureKind; 7] = [
    FailureKind::Incompatible,
    FailureKind::Capacity,
    FailureKind::Quota,
    FailureKind::OwnedElsewhere,
    FailureKind::StartupFailed,
    FailureKind::Cancelled,
    FailureKind::Lost,
];
pub const STOP_REASONS: [&str; 5] = ["deleted", "evicted", "requested", "shutdown", "restarting"];
pub const JOB_OUTCOMES: [&str; 5] = ["ok", "failed", "cancelled", "deadline_exceeded", "rejected"];

impl Metrics {
    /// Every series that can exist, at zero, so `increase()` works from the first scrape.
    pub fn new() -> Metrics {
        let m = Metrics::default();
        m.add("alephgrid_allocation_starts_total", &[], 0);
        m.add("alephgrid_allocations_lost_total", &[], 0);
        for k in FAILURES {
            m.add("alephgrid_allocation_start_failures_total", &[("reason", failure_label(k))], 0);
        }
        for r in STOP_REASONS {
            m.add("alephgrid_allocation_stops_total", &[("reason", r)], 0);
        }
        for class in [Class::Interactive, Class::Bulk] {
            for outcome in JOB_OUTCOMES {
                m.add("alephgrid_jobs_total", &[("class", class_label(class)), ("outcome", outcome)], 0);
            }
            for lane in [Lane::Read, Lane::Write] {
                let labels = [("class", class_label(class)), ("lane", lane_label(lane))];
                m.histogram("alephgrid_job_wait_seconds", &labels);
                m.histogram("alephgrid_job_duration_seconds", &labels);
            }
        }
        m.histogram("alephgrid_allocation_start_seconds", &[]);
        m.add("alephgrid_websocket_connections_total", &[], 0);
        for r in ["ok", "failed"] {
            m.add("alephgrid_imports_total", &[("result", r)], 0);
        }
        m.add("alephgrid_import_bytes_total", &[], 0);
        m.add("alephgrid_disk_reserve_releases_total", &[], 0);
        m
    }

    /// Set a counter kept elsewhere (read at scrape time).
    pub fn set(&self, name: &'static str, labels: &[(&'static str, &'static str)], v: u64) {
        self.counters.lock().unwrap_or_else(|e| e.into_inner()).insert((name, labels.to_vec()), v);
    }

    pub fn add(&self, name: &'static str, labels: &[(&'static str, &'static str)], n: u64) {
        *self.counters.lock().unwrap_or_else(|e| e.into_inner()).entry((name, labels.to_vec())).or_default() += n;
    }

    pub fn inc(&self, name: &'static str, labels: &[(&'static str, &'static str)]) {
        self.add(name, labels, 1);
    }

    fn histogram(&self, name: &'static str, labels: &[(&'static str, &'static str)]) {
        self.histograms.lock().unwrap_or_else(|e| e.into_inner()).entry((name, labels.to_vec())).or_default();
    }

    pub fn observe(&self, name: &'static str, labels: &[(&'static str, &'static str)], d: Duration) {
        let secs = d.as_secs_f64();
        let mut h = self.histograms.lock().unwrap_or_else(|e| e.into_inner());
        let e = h.entry((name, labels.to_vec())).or_default();
        let i = BUCKETS.iter().position(|b| secs <= *b).unwrap_or(BUCKETS.len());
        e.buckets[i] += 1;
        e.sum += secs;
        e.count += 1;
    }

    /// A counter's value (tests, diagnostics).
    pub fn counter(&self, name: &str, labels: &[(&str, &str)]) -> u64 {
        let c = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        c.iter().find(|((n, l), _)| *n == name && l.len() == labels.len() && l.iter().zip(labels).all(|(a, b)| a.0 == b.0 && a.1 == b.1)).map(|(_, v)| *v).unwrap_or(0)
    }

    /// The text exposition: recorded families, then `gauges`.
    pub fn render(&self, gauges: &[Gauge]) -> String {
        let mut out = String::new();
        let counters = self.counters.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let histograms = self.histograms.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for (name, kind, help) in HELP {
            out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
            if *kind == "counter" {
                for ((_, labels), v) in counters.iter().filter(|((n, _), _)| n == name) {
                    out.push_str(&format!("{name}{} {v}\n", fmt_labels(labels, None)));
                }
            } else {
                for ((_, labels), h) in histograms.iter().filter(|((n, _), _)| n == name) {
                    let mut cumulative = 0;
                    for (i, b) in BUCKETS.iter().enumerate() {
                        cumulative += h.buckets[i];
                        out.push_str(&format!("{name}_bucket{} {cumulative}\n", fmt_labels(labels, Some(&b.to_string()))));
                    }
                    out.push_str(&format!("{name}_bucket{} {}\n", fmt_labels(labels, Some("+Inf")), h.count));
                    out.push_str(&format!("{name}_sum{} {}\n", fmt_labels(labels, None), h.sum));
                    out.push_str(&format!("{name}_count{} {}\n", fmt_labels(labels, None), h.count));
                }
            }
        }
        for g in gauges {
            out.push_str(&format!("# HELP {} {}\n# TYPE {} gauge\n", g.name, g.help, g.name));
            for (labels, v) in &g.values {
                out.push_str(&format!("{}{} {v}\n", g.name, fmt_labels(labels, None)));
            }
        }
        out
    }
}

/// A gauge family read at scrape time.
pub struct Gauge {
    pub name: &'static str,
    pub help: &'static str,
    pub values: Vec<(Labels, f64)>,
}

fn gauge(name: &'static str, help: &'static str, v: f64) -> Gauge {
    Gauge { name, help, values: vec![(vec![], v)] }
}

fn fmt_labels(labels: &[(&'static str, &'static str)], le: Option<&str>) -> String {
    let mut parts: Vec<String> = labels.iter().map(|(k, v)| format!("{k}=\"{}\"", escape(v))).collect();
    if let Some(le) = le {
        parts.push(format!("le=\"{le}\""));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{{{}}}", parts.join(","))
    }
}

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn b(v: bool) -> f64 {
    if v {
        1.0
    } else {
        0.0
    }
}

/// Everything read at scrape time (blocking: readiness writes a small file).
pub fn gauges(state: &Shared) -> Vec<Gauge> {
    let c = &state.controller;
    let ready = crate::health::readiness(state);
    let (budget, reserved, uploads, upload_budget) = c.budget_use();
    let live = c.allocation_counts();
    let (waiting_read, waiting_write, running) = c.job_counts();
    // Kept by the reserve itself; it survives the reserve being recreated in between scrapes.
    state.metrics.set("alephgrid_disk_reserve_releases_total", &[], c.disk_reserve.releases());
    let mut g = vec![
        Gauge { name: "alephgrid_build_info", help: "The running version (always 1).", values: vec![(vec![("version", env!("CARGO_PKG_VERSION"))], 1.0)] },
        gauge("alephgrid_start_time_seconds", "When the process started (Unix time).", state.started_at_ms as f64 / 1000.0),
        gauge("alephgrid_ready", "1 when /readyz answers 200.", b(ready.ready())),
        gauge("alephgrid_shutting_down", "1 once shutdown has begun.", b(!ready.accepting_work)),
        Gauge {
            name: "alephgrid_disk_reserve_state",
            help: "The disk reserve: held, released (the disk filled up) or off (one of them is 1).",
            values: ["held", "released", "off"].iter().map(|s| (vec![("state", *s)], b(ready.disk_reserve == *s))).collect(),
        },
        gauge("alephgrid_disk_reserve_bytes", "Configured size of the disk reserve.", c.disk_reserve.bytes() as f64),
        gauge("alephgrid_disk_free_bytes", "Free disk where the data directory is.", crate::backup::free_bytes(&state.config.data_dir).unwrap_or(0) as f64),
        gauge("alephgrid_memory_budget_bytes", "Memory shared by open workbooks (--memory-budget).", budget.memory_bytes as f64),
        gauge("alephgrid_memory_reserved_bytes", "Memory reserved by open workbooks.", reserved.memory_bytes as f64),
        gauge("alephgrid_threads_budget", "Engine threads shared by open workbooks.", budget.threads as f64),
        gauge("alephgrid_threads_reserved", "Engine threads reserved by open workbooks.", reserved.threads as f64),
        gauge("alephgrid_upload_staged_bytes", "Bytes of staged uploads.", uploads as f64),
        gauge("alephgrid_upload_budget_bytes", "Most bytes of staged uploads.", upload_budget as f64),
        Gauge {
            name: "alephgrid_allocations",
            help: "Live workbook allocations by state.",
            values: LIVE_STATES.iter().map(|s| (vec![("state", state_label(*s))], live.by_state.get(s).copied().unwrap_or(0) as f64)).collect(),
        },
        gauge("alephgrid_workbooks_open", "Workbooks whose compute is ready.", live.by_state.get(&AllocationState::Ready).copied().unwrap_or(0) as f64),
        gauge("alephgrid_allocations_queued", "Starts waiting in line for host capacity.", live.queued as f64),
        Gauge {
            name: "alephgrid_jobs_waiting",
            help: "Jobs waiting in workbook queues, by lane.",
            values: vec![(vec![("lane", "read")], waiting_read as f64), (vec![("lane", "write")], waiting_write as f64)],
        },
        gauge("alephgrid_jobs_running", "Jobs running.", running as f64),
        gauge("alephgrid_websocket_connections", "Open WebSocket sessions.", state.sessions.lock().unwrap().len() as f64),
    ];
    let newest = crate::backup::list(&state.config.data_dir.join(crate::state::BACKUPS)).ok().and_then(|l| l.first().map(|b| b.created_at_ms));
    g.push(gauge(
        "alephgrid_last_backup_timestamp_seconds",
        "When the newest online backup in the data directory was made (Unix time; 0: none).",
        newest.unwrap_or(0) as f64 / 1000.0,
    ));
    g
}

/// Whether the request may read metrics: the metrics token, or an instance admin.
fn authorize(state: &Shared, headers: &HeaderMap) -> Result<(), StatusCode> {
    if let Some(token) = &state.config.metrics_token {
        let bearer = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(str::trim);
        if bearer.is_some_and(|b| same_secret(b, token)) {
            return Ok(());
        }
    }
    match state.auth.authenticate(headers) {
        Ok(p) if p.is_instance_admin() => Ok(()),
        Ok(_) => Err(StatusCode::FORBIDDEN),
        Err(_) => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Compare two secrets in time independent of where they differ.
fn same_secret(a: &str, b: &str) -> bool {
    use sha2::{Digest, Sha256};
    let (x, y) = (Sha256::digest(a.as_bytes()), Sha256::digest(b.as_bytes()));
    x.iter().zip(y.iter()).fold(0u8, |acc, (p, q)| acc | (p ^ q)) == 0
}

/// `GET /metrics`.
pub async fn endpoint(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let s = state.clone();
    let out = tokio::task::spawn_blocking(move || {
        authorize(&s, &headers)?;
        Ok::<_, StatusCode>(s.metrics.render(&gauges(&s)))
    })
    .await
    .unwrap_or(Err(StatusCode::INTERNAL_SERVER_ERROR));
    match out {
        Ok(text) => {
            let mut res = text.into_response();
            res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"));
            res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            res
        }
        Err(code) => {
            let msg = if code == StatusCode::FORBIDDEN { "metrics are for instance admins or the metrics token" } else { "sign in as an instance admin, or send the metrics token" };
            (code, axum::Json(serde_json::json!({ "error": msg }))).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histograms_are_cumulative_and_counters_start_at_zero() {
        let m = Metrics::new();
        m.observe("alephgrid_allocation_start_seconds", &[], Duration::from_millis(30));
        m.observe("alephgrid_allocation_start_seconds", &[], Duration::from_secs(7200));
        let text = m.render(&[]);
        // 0.03 s falls in the 0.05 bucket and every larger one; 7200 s only in +Inf.
        assert!(text.contains("alephgrid_allocation_start_seconds_bucket{le=\"0.025\"} 0\n"), "{text}");
        assert!(text.contains("alephgrid_allocation_start_seconds_bucket{le=\"0.05\"} 1\n"), "{text}");
        assert!(text.contains("alephgrid_allocation_start_seconds_bucket{le=\"3600\"} 1\n"), "{text}");
        assert!(text.contains("alephgrid_allocation_start_seconds_bucket{le=\"+Inf\"} 2\n"), "{text}");
        assert!(text.contains("alephgrid_allocation_start_seconds_count 2\n"), "{text}");
        assert!(text.contains("alephgrid_allocation_start_seconds_sum 7200.03\n"), "{text}");
        assert!(text.contains("alephgrid_allocation_start_failures_total{reason=\"quota\"} 0\n"), "{text}");
        assert!(text.contains("alephgrid_jobs_total{class=\"bulk\",outcome=\"rejected\"} 0\n"), "{text}");
    }
}
