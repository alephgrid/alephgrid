//! Support diagnostics and logging setup (9.13, docs/OPERATIONS.md#support-diagnostics).
//!
//! A diagnostics bundle is one JSON document an operator can attach to a support request:
//! version and build, platform, the effective configuration with every secret replaced by
//! `set`/`not set`, health, storage formats, sizes and counts, metrics, and the server's
//! recent log lines. Outside the log lines it holds no workbook, space or user names or ids,
//! emails, file names, paths, SQL text or cell values. Log lines are as the server wrote
//! them (never request bodies, headers or query strings) with configured secrets, tokens,
//! cookies, emails and the data directory's path cut out; they keep workbook ids and paths
//! of requests, which match the operator's own log.
//!
//! * `GET /api/admin/diagnostics` (instance admins): from the running server.
//! * `alephgrid diagnose --data-dir DIR [--out FILE]`: from the data directory alone, with the
//!   configuration the same flags and environment give; it reads files only (never a
//!   database), so it works whether or not a server is running. It has no log lines: only a
//!   running server keeps them.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;

use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use serde_json::{json, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::auth::Principal;
use crate::http::{ApiResult, blocking};
use crate::state::{Config, Shared};

/// Version of the bundle's layout.
pub const BUNDLE_FORMAT: u32 = 1;
/// Log lines kept in memory for the bundle.
pub const RECENT_LINES: usize = 200;
/// Longest log line kept (bytes); the rest is cut.
const LINE_BYTES: usize = 1000;

// ------------------------------------------------------------------ logging

static RECENT: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

fn filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| "alephgrid=info,tower_http=warn".into())
}

/// Logs go to standard output, as text or one JSON object per line, and the newest
/// [`RECENT_LINES`] are also kept for the diagnostics bundle. They never carry secrets,
/// tokens, cookies or query strings: requests are logged by id, method and path only.
pub fn init_logging(json: bool) {
    let text = (!json).then(tracing_subscriber::fmt::layer);
    let json = json.then(|| tracing_subscriber::fmt::layer().json().flatten_event(true).with_current_span(false).with_span_list(true));
    tracing_subscriber::registry().with(filter()).with(text).with(json).with(RecentLogs).init();
}

/// Keep recent log lines without printing them (tests). False when logging was set up already.
pub fn init_recent_logs_only() -> bool {
    tracing_subscriber::registry().with(filter()).with(RecentLogs).try_init().is_ok()
}

/// The newest log lines, oldest first, as kept (not yet scrubbed).
pub fn recent_lines() -> Vec<String> {
    RECENT.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
}

/// A tracing layer keeping the newest [`RECENT_LINES`] lines: time, level, target, message,
/// the event's fields and its spans' fields (request id, workbook, job).
pub struct RecentLogs;

#[derive(Default)]
struct Fields {
    message: String,
    rest: String,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.put(field, format!("{value:?}"));
    }
}

impl Fields {
    fn put(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = value;
        } else {
            if !self.rest.is_empty() {
                self.rest.push(' ');
            }
            self.rest.push_str(&format!("{}={value}", field.name()));
        }
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for RecentLogs {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut f = Fields::default();
        attrs.record(&mut f);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(f);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut f = Fields::default();
        event.record(&mut f);
        let mut line = format!("{} {} {}: {}", alephgrid_core::storage::now_ms(), meta.level(), meta.target(), f.message);
        if !f.rest.is_empty() {
            line.push(' ');
            line.push_str(&f.rest);
        }
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(sf) = span.extensions().get::<Fields>() {
                    line.push_str(&format!(" {}{{{}}}", span.name(), sf.rest));
                }
            }
        }
        if line.len() > LINE_BYTES {
            let mut cut = LINE_BYTES;
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            line.truncate(cut);
            line.push('…');
        }
        let mut recent = RECENT.lock().unwrap_or_else(|e| e.into_inner());
        if recent.len() >= RECENT_LINES {
            recent.pop_front();
        }
        recent.push_back(line);
    }
}

// ---------------------------------------------------------------- redaction

/// The configured secrets and identities, to cut out of anything quoted in the bundle.
fn secrets(config: &Config) -> Vec<String> {
    let mut s = Vec::new();
    if let Some((email, password)) = &config.auth.bootstrap {
        s.push(email.clone());
        s.push(password.clone());
    }
    if let Some(o) = &config.auth.oidc {
        s.extend(o.client_secret.clone());
        s.push(o.client_id.clone());
    }
    s.extend(config.metrics_token.clone());
    s.retain(|x| x.len() >= 4);
    s
}

/// A log line as it may go into the bundle: configured secrets, tokens, session cookies,
/// bearer credentials and email addresses replaced, and the data directory's path shortened.
pub fn scrub(line: &str, secrets: &[String], data_dir: &Path) -> String {
    let mut out = line.to_string();
    for s in secrets {
        out = out.replace(s.as_str(), "[redacted]");
    }
    let dir = data_dir.display().to_string();
    if dir.len() > 1 {
        out = out.replace(&dir, "<data-dir>");
    }
    for prefix in [crate::auth::TOKEN_PREFIX, crate::mcp::AGENT_PREFIX, "alephgrid_session=", "Bearer "] {
        out = cut_after(&out, prefix);
    }
    redact_emails(&out)
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_-.~+/=".contains(c)
}

/// Replace what follows each `prefix` (a token's characters) with `[redacted]`.
fn cut_after(s: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(prefix) {
        out.push_str(&rest[..i + prefix.len()]);
        let after = &rest[i + prefix.len()..];
        let n = after.find(|c: char| !token_char(c)).unwrap_or(after.len());
        if n > 0 {
            out.push_str("[redacted]");
        }
        rest = &after[n..];
    }
    out.push_str(rest);
    out
}

fn redact_emails(s: &str) -> String {
    let local = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let domain = |c: char| c.is_ascii_alphanumeric() || ".-".contains(c);
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let mut start = out.len();
            while start > 0 && out[..start].chars().next_back().is_some_and(local) {
                start -= out[..start].chars().next_back().unwrap().len_utf8();
            }
            let mut end = i + 1;
            while end < chars.len() && domain(chars[end]) {
                end += 1;
            }
            let host: String = chars[i + 1..end].iter().collect();
            if start < out.len() && host.trim_end_matches('.').contains('.') {
                out.truncate(start);
                out.push_str("[email]");
                i = end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn set(b: bool) -> &'static str {
    if b {
        "set"
    } else {
        "not set"
    }
}

/// The effective configuration: limits as they are; secrets, identities, paths and URLs
/// only as `set` / `not set`.
pub fn redacted_config(c: &Config) -> Value {
    let oidc = c.auth.oidc.as_ref().map(|o| {
        json!({
            "issuer": "set",
            "client_id": "set",
            "client_secret": set(o.client_secret.is_some()),
            "scopes": o.scopes,
            "groups_claim": o.groups_claim,
            "join": format!("{:?}", o.join).to_ascii_lowercase(),
        })
    });
    let public_url = c.auth.public_url.as_deref().map(|u| if u.starts_with("https://") { "set (https)" } else { "set (http)" });
    json!({
        "auth": {
            "mode": c.auth.mode,
            "public_url": public_url.unwrap_or("not set"),
            "session_hours": c.auth.session_ttl.as_secs() / 3600,
            "allowed_hosts": c.auth.allowed_hosts.as_ref().map(|h| h.len()),
            "bootstrap_admin": set(c.auth.bootstrap.is_some()),
            "oidc": oidc,
        },
        "metrics_token": set(c.metrics_token.is_some()),
        "allow_sql": c.allow_sql,
        "memory_budget_bytes": c.memory_budget,
        "threads": c.threads,
        "temp_budget_bytes": c.temp_budget,
        "disk_reserve_bytes": c.disk_reserve,
        "min_workbook_memory_bytes": c.min_workbook_memory,
        "idle_timeout_seconds": c.idle_timeout.as_secs(),
        "profiles": c.profiles,
        "space_defaults": c.quota,
        "python_runtime": set(c.python_runtime.is_some()),
        "python_isolation": if c.python_isolation.is_some() { "none" } else { "auto" },
        "python_limits": c.python_limits,
        "start_queue_timeout_seconds": c.start_queue_timeout.as_secs(),
        "view_ttl_seconds": c.view_ttl.as_secs(),
        "shutdown_grace_seconds": c.shutdown_grace.as_secs(),
        "max_message_bytes": c.max_message_bytes,
        "max_upload_bytes": c.max_upload_bytes,
        "max_workbook_bytes": c.max_workbook_bytes,
        "backups_kept": c.backups_kept,
        "backup_interval_seconds": c.backup_interval.as_secs(),
        "request_timeout_seconds": c.request_timeout.as_secs(),
        "job_timeout_seconds": c.job_timeout.as_secs(),
        "outbox_pause_bytes": c.outbox_pause_bytes,
        "outbox_max_bytes": c.outbox_max_bytes,
        "sheet_copy_rows": c.sheet_copy_rows,
    })
}

// ------------------------------------------------------------------- facts

pub fn build() -> Value {
    let spec = crate::runtime::RuntimeSpec::current();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "storage_format": alephgrid_core::storage::FORMAT,
        "runtime_spec": spec.id(),
        "components": spec.components,
        "debug_build": cfg!(debug_assertions),
    })
}

pub fn platform() -> Value {
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "memory_bytes": crate::runtime::budget::host_memory(),
    })
}

/// Bytes of the files under `dir`, links not followed.
fn tree_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.path().symlink_metadata() {
            Ok(m) if m.is_dir() => tree_size(&e.path()),
            Ok(m) if m.is_file() => m.len(),
            _ => 0,
        })
        .sum()
}

/// Counts and sizes in the data directory, from files only (no database is opened).
pub fn storage(data_dir: &Path) -> Value {
    let control = data_dir.join(".control");
    let (mut count, mut unreadable, mut bytes, mut largest) = (0u64, 0u64, 0u64, 0u64);
    let mut formats: BTreeMap<String, u64> = BTreeMap::new();
    for e in std::fs::read_dir(data_dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let dir = e.path();
        if name.starts_with('.') || !crate::state::valid_id(&name) || !dir.join("workbook.json").is_file() {
            continue;
        }
        count += 1;
        let db = crate::state::database_bytes(&dir);
        bytes += db;
        largest = largest.max(db);
        let format = match crate::state::read_meta(&dir) {
            Ok(meta) => match meta.runtime_spec {
                Some(id) => crate::runtime::load_spec(&control, &id).map(|s| s.storage_format.to_string()).unwrap_or_else(|_| "unknown".into()),
                None => "never_opened".into(),
            },
            Err(_) => {
                unreadable += 1;
                "unknown".into()
            }
        };
        *formats.entry(format).or_default() += 1;
    }
    let trash = data_dir.join(crate::state::TRASH);
    let trashed = std::fs::read_dir(&trash).map(|d| d.flatten().filter(|e| e.path().is_dir()).count()).unwrap_or(0);
    let mut states: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut failures: BTreeMap<&'static str, u64> = BTreeMap::new();
    for e in std::fs::read_dir(control.join("allocations")).into_iter().flatten().flatten() {
        let Ok(record) = std::fs::read(e.path()).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice::<crate::runtime::RuntimeAllocation>(&b)?)) else { continue };
        *states.entry(crate::metrics::state_label(record.state)).or_default() += 1;
        if let Some(f) = record.failure {
            *failures.entry(crate::metrics::failure_label(f.kind)).or_default() += 1;
        }
    }
    let backups = crate::backup::list(&data_dir.join(crate::state::BACKUPS)).unwrap_or_default();
    let now = alephgrid_core::storage::now_ms();
    let scratch: BTreeMap<&str, u64> =
        [crate::state::UPLOADS, crate::state::EXPORTS, ".tmp", ".python", ".agent-results"].into_iter().map(|d| (d, tree_size(&data_dir.join(d)))).collect();
    let control_db = control.join("control.duckdb");
    let control_db_bytes = std::fs::metadata(&control_db).map(|m| m.len()).unwrap_or(0) + std::fs::metadata(control_db.with_extension("duckdb.wal")).map(|m| m.len()).unwrap_or(0);
    json!({
        "workbooks": count,
        "workbooks_unreadable": unreadable,
        "workbook_database_bytes": bytes,
        "largest_workbook_database_bytes": largest,
        "workbooks_by_storage_format": formats,
        "trash": { "workbooks": trashed, "bytes": tree_size(&trash) },
        "allocation_records": { "by_state": states, "by_failure": failures },
        "control_database_bytes": control_db_bytes,
        "backups": {
            "count": backups.len(),
            "bytes": backups.iter().map(|b| b.bytes).sum::<u64>(),
            "newest_age_seconds": backups.first().map(|b| now.saturating_sub(b.created_at_ms) / 1000),
        },
        "scratch_bytes": scratch,
        "disk_free_bytes": crate::backup::free_bytes(data_dir),
        "disk_reserve_file": control.join("reserve").is_file(),
    })
}

fn logs(lines: Vec<String>, config: &Config, note: &str) -> Value {
    let secrets = secrets(config);
    let lines: Vec<String> = lines.iter().map(|l| scrub(l, &secrets, &config.data_dir)).collect();
    json!({ "lines": lines, "note": note })
}

/// The bundle of a running server (blocking: it probes readiness and reads the disk).
pub fn online(state: &Shared) -> Value {
    let ready = crate::health::readiness(state);
    let counts = state.controller.allocation_counts();
    let (waiting_read, waiting_write, running) = state.controller.job_counts();
    let store = &state.auth.store;
    let now = alephgrid_core::storage::now_ms();
    json!({
        "bundle": "alephgrid-diagnostics",
        "format": BUNDLE_FORMAT,
        "source": "server",
        "generated_at_ms": now,
        "uptime_seconds": now.saturating_sub(state.started_at_ms) / 1000,
        "build": build(),
        "platform": platform(),
        "config": redacted_config(&state.config),
        "health": {
            "ready": ready.ready(),
            "accepting_work": ready.accepting_work,
            "data_dir_writable": ready.data_dir_writable,
            "control_db": ready.control_db,
            "disk_reserve": ready.disk_reserve,
        },
        "runtime": {
            "allocations": counts.by_state.iter().map(|(s, n)| (crate::metrics::state_label(*s), *n)).collect::<BTreeMap<_, _>>(),
            "allocations_queued": counts.queued,
            "jobs_waiting": { "read": waiting_read, "write": waiting_write },
            "jobs_running": running,
            "websocket_connections": state.sessions.lock().unwrap().len(),
            "python_available": state.python.is_some(),
        },
        "accounts": {
            "users": store.user_count().ok(),
            "organizations": store.orgs().ok().map(|o| o.len()),
            "spaces": store.spaces().ok().map(|s| s.len()),
            "agent_connections": store.agents().ok().map(|a| a.len()),
        },
        "storage": storage(&state.config.data_dir),
        "metrics": state.metrics.render(&crate::metrics::gauges(state)),
        "logs": logs(recent_lines(), &state.config, "the newest log lines of this process, with secrets, tokens, emails and paths cut out"),
    })
}

/// The bundle from the data directory alone (`alephgrid diagnose`), with `config` the
/// configuration of this command's flags and environment.
pub fn offline(config: &Config) -> Value {
    let control = config.data_dir.join(".control");
    // A running server holds this lock; taking it here would be released at once.
    let server_running = std::fs::OpenOptions::new()
        .write(true)
        .open(control.join("controller.lock"))
        .is_ok_and(|f| matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
    let probe_dir = if control.is_dir() { control } else { config.data_dir.clone() };
    json!({
        "bundle": "alephgrid-diagnostics",
        "format": BUNDLE_FORMAT,
        "source": "offline",
        "generated_at_ms": alephgrid_core::storage::now_ms(),
        "build": build(),
        "platform": platform(),
        "config": redacted_config(config),
        "health": {
            "server_running": server_running,
            "data_dir_writable": crate::health::probe_write(&probe_dir),
        },
        "storage": storage(&config.data_dir),
        "logs": logs(vec![], config, "only a running server keeps recent log lines: an instance admin gets them from GET /api/admin/diagnostics"),
    })
}

/// `GET /api/admin/diagnostics` (instance admins): the bundle as a download.
pub async fn endpoint(State(state): State<Shared>, Extension(p): Extension<Principal>) -> ApiResult<Response> {
    state.auth.require_instance_admin(&p)?;
    let s = state.clone();
    let bundle = blocking(move || Ok(online(&s))).await?;
    state.auth.audit(&p, "diagnostics_downloaded", None, None, None, json!({}));
    let name = format!("attachment; filename=\"alephgrid-diagnostics-{}.json\"", alephgrid_core::storage::now_ms());
    let mut res = axum::Json(bundle).into_response();
    if let Ok(v) = HeaderValue::from_str(&name) {
        res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_cuts_secrets_tokens_emails_and_the_data_directory() {
        let secrets = vec!["hunter2-oidc-secret".to_string()];
        let line = "warn x: failed for ann.lee+x@corp.example.com with agt_abc123.def and agc_Zz9 \
                    cookie alephgrid_session=s3ss10n Bearer t0k3n secret hunter2-oidc-secret at /srv/data/ab12/data.duckdb";
        let out = scrub(line, &secrets, Path::new("/srv/data"));
        assert_eq!(
            out,
            "warn x: failed for [email] with agt_[redacted] and agc_[redacted] \
             cookie alephgrid_session=[redacted] Bearer [redacted] secret [redacted] at <data-dir>/ab12/data.duckdb"
        );
        // Not an address: no dot after the @, or nothing before it.
        assert_eq!(scrub("x@y and @home.example", &[], Path::new("/d")), "x@y and @home.example");
    }
}
