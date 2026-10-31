//! REST endpoints, auth and the embedded web UI.

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State, WebSocketUpgrade};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Extension, Json, Router};
use futures_util::StreamExt;
use alephgrid_core::{ColumnType, ExportFormat, ImportProblem, UploadPreview, ViewSpec};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use std::collections::HashMap;

use anyhow::bail;

use crate::auth::{Cap, Denied, Principal};
use crate::runtime::{FailureKind, RuntimeFailure};
use crate::sched::Class;
use crate::state::Work;
use crate::state::{Shared, WorkbookPatch};
use crate::ws;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct Assets;

/// An error reply: `{"error": message}`, plus `problem` when an import can say what to fix.
pub struct ApiError(pub(crate) StatusCode, pub(crate) String, pub(crate) Option<Box<ImportProblem>>);

impl ApiError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> ApiError {
        ApiError(status, message.into(), None)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.1 });
        if let Some(p) = self.2 {
            body["problem"] = json!(p);
        }
        (self.0, Json(body)).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        if let Some(q) = e.chain().find_map(|c| c.downcast_ref::<crate::quota::QuotaExceeded>()) {
            let status = match q.kind {
                crate::quota::QuotaKind::Storage | crate::quota::QuotaKind::Workbooks => StatusCode::INSUFFICIENT_STORAGE,
                crate::quota::QuotaKind::Jobs | crate::quota::QuotaKind::OpenWorkbooks => StatusCode::TOO_MANY_REQUESTS,
            };
            return ApiError::new(status, q.message.clone());
        }
        if let Some(d) = e.chain().find_map(|c| c.downcast_ref::<Denied>()) {
            let status = match d {
                Denied::Unauthenticated => StatusCode::UNAUTHORIZED,
                Denied::Forbidden(_) | Denied::Refused(_) => StatusCode::FORBIDDEN,
                Denied::NotFound(_) => StatusCode::NOT_FOUND,
                Denied::BadRequest(_) => StatusCode::BAD_REQUEST,
            };
            return ApiError::new(status, d.to_string());
        }
        if let Some(f) = e.chain().find_map(|c| c.downcast_ref::<RuntimeFailure>()) {
            return ApiError(runtime_status(f.kind), f.message.clone(), None);
        }
        if let Some(j) = e.chain().find_map(|c| c.downcast_ref::<crate::sched::JobError>()) {
            let status = match j {
                crate::sched::JobError::Overloaded(_) => StatusCode::SERVICE_UNAVAILABLE,
                crate::sched::JobError::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
                crate::sched::JobError::Cancelled => StatusCode::CONFLICT,
            };
            return ApiError::new(status, j.to_string());
        }
        if e.chain().any(|c| c.downcast_ref::<alephgrid_core::limits::TooLarge>().is_some()) {
            return ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, format!("{e:#}"));
        }
        let text = format!("{e:#}");
        if text.starts_with("workbook ") && text.contains(" not found") || text.contains("is not in the trash") {
            return ApiError::new(StatusCode::NOT_FOUND, text);
        }
        let problem = e.chain().find_map(|c| c.downcast_ref::<ImportProblem>()).cloned().map(Box::new);
        let message = match &problem {
            Some(p) => p.message.clone(),
            None if format!("{e:#}").contains("INTERRUPT") => "Cancelled.".to_string(),
            None => format!("{e:#}"),
        };
        ApiError(StatusCode::BAD_REQUEST, message, problem)
    }
}

/// Reply to a staged upload: its id and what importing it would produce.
#[derive(Serialize, TS)]
pub struct UploadCreated {
    pub upload_id: String,
    pub filename: String,
    pub size_bytes: u64,
    pub preview: UploadPreview,
}

/// Body of a preview or import request for a staged upload.
#[derive(Deserialize, TS)]
#[ts(optional_fields)]
pub struct ImportRequest {
    /// Table name (tables only); defaults to the file name.
    pub table: Option<String>,
    #[serde(default)]
    pub types: Vec<ColumnType>,
    /// Client-chosen id for `GET/DELETE /api/workbooks/{id}/jobs/{job}` while it runs.
    pub job: Option<String>,
}

/// Progress of a running job.
#[derive(Serialize, TS)]
pub struct JobStatus {
    pub running: bool,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percentage: Option<f64>,
    pub rows_processed: u64,
    pub total_rows: u64,
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;

/// HTTP status for a runtime failure.
fn runtime_status(kind: FailureKind) -> StatusCode {
    match kind {
        FailureKind::Incompatible => StatusCode::CONFLICT,
        FailureKind::Capacity | FailureKind::StartupFailed | FailureKind::Lost => StatusCode::SERVICE_UNAVAILABLE,
        FailureKind::OwnedElsewhere => StatusCode::LOCKED,
        FailureKind::Cancelled => StatusCode::CONFLICT,
    }
}

/// `POST /api/workbooks/{id}/runtime`.
#[derive(Deserialize, TS)]
pub struct RuntimeRequest {
    pub action: RuntimeAction,
    /// Repeating an operation id that already moved this workbook's allocation returns the
    /// current status and does nothing.
    #[serde(default)]
    #[ts(optional, as = "Option<String>")]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAction {
    Start,
    Stop,
    /// Cancel a start that has not reached `ready`.
    Cancel,
}

pub fn router(state: Shared, web_dir: Option<PathBuf>) -> Router {
    let api = Router::new()
        .route("/api/me", get(crate::auth::routes::me))
        .route("/api/me/password", post(crate::auth::routes::change_password))
        .route("/api/workbooks", get(list_workbooks).post(create_workbook))
        .route("/api/workbooks/{id}", get(get_workbook).patch(update_workbook).delete(delete_workbook))
        .route("/api/workbooks/{id}/duplicate", post(duplicate_workbook))
        .route("/api/workbooks/{id}/export.xlsx", get(export_xlsx))
        .route("/api/trash", get(list_trash).delete(empty_trash))
        .route("/api/trash/{id}", axum::routing::delete(purge_workbook))
        .route("/api/trash/{id}/restore", post(restore_workbook))
        .route("/api/spaces", get(crate::auth::routes::list_spaces).post(crate::auth::routes::create_space))
        .route("/api/spaces/{id}", patch(crate::auth::routes::rename_space).delete(crate::auth::routes::delete_space))
        .route("/api/spaces/{id}/members", get(crate::auth::routes::space_members).put(crate::auth::routes::set_space_member))
        .route("/api/spaces/{id}/members/{kind}/{principal}", axum::routing::delete(crate::auth::routes::remove_space_member))
        .route("/api/spaces/{id}/tokens", get(crate::auth::routes::list_tokens).post(crate::auth::routes::create_token))
        .route("/api/spaces/{id}/tokens/{token}", axum::routing::delete(crate::auth::routes::revoke_token))
        .route("/api/orgs", get(crate::auth::routes::list_orgs).post(crate::auth::routes::create_org))
        .route("/api/orgs/{id}", patch(crate::auth::routes::rename_org))
        .route("/api/orgs/{id}/members", get(crate::auth::routes::org_members))
        .route("/api/orgs/{id}/members/{user}", patch(crate::auth::routes::set_org_role).delete(crate::auth::routes::remove_org_member))
        .route("/api/orgs/{id}/invites", get(crate::auth::routes::list_invites).post(crate::auth::routes::create_invite))
        .route("/api/orgs/{id}/invites/{invite}", axum::routing::delete(crate::auth::routes::revoke_invite))
        .route("/api/orgs/{id}/audit", get(crate::auth::routes::org_audit))
        .route("/api/orgs/{id}/usage", get(crate::auth::routes::org_usage))
        .route("/api/spaces/{id}/quota", axum::routing::put(crate::auth::routes::set_space_quota))
        .route("/api/admin/usage", get(crate::auth::routes::instance_usage))
        .route("/api/spaces/{id}/library", get(crate::library::list).post(crate::library::publish_entry))
        .route("/api/spaces/{id}/library/{entry}", get(crate::library::entry).patch(crate::library::change_entry))
        .route("/api/workbooks/{id}/library", post(crate::library::use_entry))
        .route("/api/workbooks/{id}/shareable", get(shareable))
        .route("/api/users/{id}", patch(crate::auth::routes::update_user))
        .route(
            "/api/workbooks/{id}/import",
            post(import_file).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/workbooks/{id}/export", get(export))
        .route("/api/workbooks/{id}/downloads/{download}", get(download))
        .route("/api/workbooks/{id}/uploads", post(stage_upload).layer(DefaultBodyLimit::disable()))
        .route("/api/workbooks/{id}/uploads/{upload}", axum::routing::delete(cancel_upload))
        .route("/api/workbooks/{id}/uploads/{upload}/preview", post(preview_upload))
        .route("/api/workbooks/{id}/uploads/{upload}/import", post(import_upload))
        .route("/api/workbooks/{id}/jobs/{job}", get(job_status).delete(cancel_job))
        .route("/api/workbooks/{id}/runtime", get(runtime_status_handler).post(runtime_action))
        .route("/api/admin/resources", get(resources))
        .route("/api/admin/sessions", get(sessions))
        .route("/api/admin/backups", post(create_backup))
        .route("/api/admin/audit", get(crate::auth::routes::instance_audit))
        .route("/ws/{id}", get(websocket))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));

    // Reachable without signing in.
    let public = Router::new()
        .route("/api/config", get(config))
        .route("/api/auth/setup", post(crate::auth::routes::setup))
        .route("/api/auth/login", post(crate::auth::routes::login))
        .route("/api/auth/logout", post(crate::auth::routes::logout))
        .route("/api/auth/oidc/start", get(crate::auth::routes::oidc_start))
        .route("/api/auth/oidc/callback", get(crate::auth::routes::oidc_callback))
        .route("/api/invites/lookup", post(crate::auth::routes::lookup_invite))
        .route("/api/invites/accept", post(crate::auth::routes::accept_invite));

    let app = public.merge(api);
    let app = match web_dir {
        Some(dir) => app.fallback_service(
            tower_http::services::ServeDir::new(&dir)
                .fallback(tower_http::services::ServeFile::new(dir.join("index.html"))),
        ),
        None => app.fallback(static_asset),
    };
    app.layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(tower_http::trace::TraceLayer::new_for_http())
}

/// Host part of a `Host` header or URL authority, lower case, without the port.
fn host_name(authority: &str) -> String {
    let a = authority.trim().to_ascii_lowercase();
    if let Some(rest) = a.strip_prefix('[') {
        return format!("[{}]", rest.split(']').next().unwrap_or(""));
    }
    a.rsplit_once(':').filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit())).map(|(h, _)| h.to_string()).unwrap_or(a)
}

/// Checks before anything else, for every request (7.06, 7.17):
///
/// * identity headers set by a proxy are refused: Alephgrid never trusts them;
/// * the `Host` must be one this server answers to (DNS rebinding);
/// * a request from a browser (`Origin`) that changes something or opens a WebSocket must
///   come from this server's own pages; with a session cookie, a change must say where it
///   comes from at all.
async fn guard(State(state): State<Shared>, req: Request, next: Next) -> Response {
    let headers = req.headers();
    if let Some(h) = crate::auth::identity_header(headers) {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("this request carries the identity header {h}; Alephgrid does not accept identities asserted by proxies (configure OIDC instead)"),
        )
        .into_response();
    }
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    if let Some(allowed) = &state.config.auth.allowed_hosts {
        let name = host_name(host);
        if !allowed.iter().any(|a| a == &name) {
            return ApiError::new(StatusCode::MISDIRECTED_REQUEST, "this server does not answer to that host name").into_response();
        }
    }
    let method = req.method().clone();
    let upgrade = headers.get(header::UPGRADE).is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    let changes = !matches!(method, axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS);
    if changes || upgrade {
        let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
        let own = |o: &str| {
            let authority = o.split_once("://").map(|(_, a)| a).unwrap_or("");
            authority.eq_ignore_ascii_case(host) || state.config.auth.public_url.as_deref().is_some_and(|u| u.eq_ignore_ascii_case(o))
        };
        match origin {
            Some(o) if !own(o) => return ApiError::new(StatusCode::FORBIDDEN, "requests from other sites are refused").into_response(),
            None if changes && crate::auth::cookie(headers, crate::auth::SESSION_COOKIE).is_some() && headers.get(header::AUTHORIZATION).is_none() => {
                let same = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("same-origin");
                if !same {
                    return ApiError::new(StatusCode::FORBIDDEN, "a change made with a session cookie must come from this server's pages").into_response();
                }
            }
            _ => {}
        }
    }
    let res = next.run(req).await;
    if !(res.status().is_client_error() || res.status().is_server_error()) {
        return res;
    }
    // Error messages (DuckDB's included) can name files: never show where the data lives.
    let (mut parts, body) = res.into_parts();
    let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap_or_default();
    let text = crate::ws::redact(&String::from_utf8_lossy(&bytes), &state.config.data_dir);
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(text))
}

/// Who is asking: the principal goes into the request's extensions for the handler.
async fn authenticate(State(state): State<Shared>, mut req: Request, next: Next) -> Response {
    let s = state.clone();
    let headers = req.headers().clone();
    match tokio::task::spawn_blocking(move || s.auth.authenticate(&headers)).await {
        Ok(Ok(p)) => {
            req.extensions_mut().insert(p);
            next.run(req).await
        }
        Ok(Err(e)) => ApiError::from(e).into_response(),
        Err(e) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn config(State(state): State<Shared>) -> Json<serde_json::Value> {
    let auth = &state.auth;
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "auth": auth.mode(),
        "setup_required": auth.needs_setup(),
        "sso": auth.oidc.as_ref().map(|o| json!({ "label": o.config.label })),
        "allow_sql": state.config.allow_sql,
        "sheet_copy_rows": state.config.sheet_copy_rows,
        "max_message_bytes": state.config.max_message_bytes,
        "max_upload_bytes": state.config.max_upload_bytes,
        "max_edit_cells": alephgrid_core::limits::MAX_EDIT_CELLS,
    }))
}

/// The workbook's space, if `p` may do `cap` with it (off the async threads).
async fn allowed(state: &Shared, p: &Principal, id: &str, cap: Cap) -> ApiResult<(crate::auth::Space, crate::auth::SpaceRole)> {
    let (s, p, id) = (state.clone(), p.clone(), id.to_string());
    blocking(move || s.require_workbook(&p, &id, cap)).await
}

/// Workbooks in spaces the principal can see, each with its role there.
async fn list_workbooks(State(state): State<Shared>, Extension(p): Extension<Principal>) -> ApiResult<impl IntoResponse> {
    let list = blocking(move || {
        let spaces: HashMap<String, crate::auth::SpaceRole> = state.auth.visible_spaces(&p)?.into_iter().map(|(s, r)| (s.id, r)).collect();
        Ok(state
            .list()?
            .into_iter()
            .filter_map(|mut w| {
                w.role = Some(*spaces.get(&w.meta.space)?);
                Some(w)
            })
            .collect::<Vec<_>>())
    })
    .await?;
    Ok(Json(list))
}

#[derive(Deserialize)]
struct CreateBody {
    name: String,
    /// Space to create it in; by default the first space where the principal may.
    #[serde(default)]
    space: Option<String>,
}

async fn create_workbook(State(state): State<Shared>, Extension(p): Extension<Principal>, Json(body): Json<CreateBody>) -> ApiResult<impl IntoResponse> {
    let name = if body.name.trim().is_empty() { "Untitled workbook".to_string() } else { body.name };
    let meta = blocking(move || {
        let space = match body.space.filter(|s| !s.is_empty()) {
            Some(s) => state.auth.require_space(&p, &s, Cap::CreateWorkbook)?.0,
            None => {
                let default = state.auth.store.setting("default_space")?;
                let mut spaces = state.auth.visible_spaces(&p)?.into_iter().filter(|(_, r)| r.allows(Cap::CreateWorkbook)).map(|(s, _)| s).collect::<Vec<_>>();
                spaces.sort_by_key(|s| Some(&s.id) != default.as_ref());
                spaces.into_iter().next().ok_or_else(|| crate::auth::forbidden("create workbooks: you are not an editor of any space"))?
            }
        };
        let h = state.create(&name, &space.id)?;
        let meta = h.meta.lock().unwrap().clone();
        state.auth.audit(&p, "workbook_created", Some(&space.org_id), Some(&space.id), Some(&meta.id), json!({ "name": meta.name }));
        Ok(meta)
    })
    .await?;
    Ok(Json(meta))
}

async fn duplicate_workbook(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    let (space, _) = allowed(&state, &p, &id, Cap::View).await?;
    let s = state.clone();
    let (pp, sid) = (p.clone(), space.id.clone());
    blocking(move || s.auth.require_space(&pp, &sid, Cap::CreateWorkbook)).await?;
    let meta = blocking(move || {
        let meta = state.duplicate(&id)?;
        state.auth.audit(&p, "workbook_duplicated", Some(&space.org_id), Some(&space.id), Some(&meta.id), json!({ "from": id }));
        Ok(meta)
    })
    .await?;
    Ok(Json(meta))
}

async fn get_workbook(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    let (_, role) = allowed(&state, &p, &id, Cap::View).await?;
    let (meta, tables, sheets) = blocking(move || {
        let h = state.get(&id)?;
        let meta = h.meta.lock().unwrap().clone();
        let (tables, sheets) = h.with_reader(|r| Ok((r.list_tables()?, r.sheets().list())))?;
        Ok((meta, tables, sheets))
    })
    .await?;
    Ok(Json(json!({ "workbook": meta, "role": role, "tables": tables, "sheets": sheets })))
}

async fn runtime_status_handler(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    if !crate::state::valid_id(&id) {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid workbook id"));
    }
    allowed(&state, &p, &id, Cap::View).await?;
    Ok(Json(blocking(move || state.controller.status(&id)).await?))
}

/// Start, stop or cancel a workbook's allocation. The workbook's data is unaffected.
async fn runtime_action(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<RuntimeRequest>,
) -> ApiResult<impl IntoResponse> {
    if !crate::state::valid_id(&id) {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid workbook id"));
    }
    // Opening is part of viewing; stopping or cancelling affects everyone using it.
    let cap = if matches!(req.action, RuntimeAction::Start) { Cap::View } else { Cap::Edit };
    allowed(&state, &p, &id, cap).await?;
    let status = blocking(move || {
        let c = &state.controller;
        let op = req.operation_id.filter(|o| !o.is_empty()).unwrap_or_else(|| crate::runtime::controller::operation_id("api"));
        if !c.seen(&id, &op) {
            match req.action {
                RuntimeAction::Start => {
                    state.get_with(&id, &op)?;
                }
                RuntimeAction::Stop => c.stop(&id, &op, crate::runtime::controller::StopReason::Requested)?,
                RuntimeAction::Cancel => {
                    c.cancel(&id);
                }
            }
        }
        c.status(&id)
    })
    .await?;
    Ok(Json(status))
}

/// Back up every workbook into `<data-dir>/.backups/<time>/`, each from one snapshot while
/// work continues. The path is chosen by the server, never by the client.
async fn create_backup(State(state): State<Shared>, Extension(p): Extension<Principal>) -> ApiResult<impl IntoResponse> {
    state.auth.require_instance_admin(&p)?;
    let out = blocking(move || {
        let dest = state.config.data_dir.join(".backups").join(format!("backup-{}", alephgrid_core::storage::now_ms()));
        let manifest = crate::backup::backup_online(&state, &dest)?;
        state.auth.audit(&p, "backup_created", None, None, None, json!({ "workbooks": manifest.workbooks.len() }));
        Ok(json!({ "path": dest, "manifest": manifest }))
    })
    .await?;
    Ok(Json(out))
}

/// Open connections with their unsent bytes (to see slow clients).
async fn sessions(State(state): State<Shared>, Extension(p): Extension<Principal>) -> ApiResult<impl IntoResponse> {
    state.auth.require_instance_admin(&p)?;
    let list: Vec<serde_json::Value> = state
        .sessions
        .lock()
        .unwrap()
        .iter()
        .map(|(id, s)| json!({ "session": id, "workbook": s.workbook, "outbox_bytes": s.outbox.bytes(), "peak_outbox_bytes": s.outbox.peak() }))
        .collect();
    Ok(Json(list))
}

async fn resources(State(state): State<Shared>, Extension(p): Extension<Principal>) -> ApiResult<impl IntoResponse> {
    state.auth.require_instance_admin(&p)?;
    Ok(Json(blocking(move || Ok(state.controller.usage())).await?))
}

/// Rename, star or move a workbook. Moving needs `delete` where it is and `create` where it goes.
async fn update_workbook(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Json(body): Json<WorkbookPatch>,
) -> ApiResult<impl IntoResponse> {
    let (space, _) = allowed(&state, &p, &id, Cap::CreateWorkbook).await?;
    let meta = blocking(move || {
        let to = match body.space.as_deref().filter(|s| *s != space.id) {
            Some(to) => {
                state.require_workbook(&p, &id, Cap::DeleteWorkbook)?;
                let (dest, _) = state.auth.require_space(&p, to, Cap::CreateWorkbook)?;
                if dest.org_id != space.org_id {
                    bail!(crate::auth::Denied::BadRequest("a workbook can move only between spaces of one organization".into()));
                }
                Some(dest)
            }
            None => None,
        };
        let meta = state.update(&id, body)?;
        if let Some(dest) = to {
            state.auth.audit(&p, "workbook_moved", Some(&space.org_id), Some(&dest.id), Some(&id), json!({ "from": space.id }));
        }
        Ok(meta)
    })
    .await?;
    Ok(Json(meta))
}

async fn delete_workbook(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    let (space, _) = allowed(&state, &p, &id, Cap::DeleteWorkbook).await?;
    blocking(move || {
        state.delete(&id)?;
        state.auth.audit(&p, "workbook_deleted", Some(&space.org_id), Some(&space.id), Some(&id), json!({}));
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// What a workbook could publish to its space's library: its saved queries, views and
/// functions (readers of the workbook may see them).
async fn shareable(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    allowed(&state, &p, &id, Cap::View).await?;
    let h = open(&state, &id).await?;
    let out = h
        .run(
            http_job(Class::Interactive, &state),
            None,
            None,
            Work::Read(Box::new(|r: &alephgrid_core::Reader| {
                Ok(json!({ "queries": r.saved_queries()?, "views": r.saved_views(None)?, "functions": r.functions()? }))
            })),
        )
        .await?;
    Ok(Json(out))
}

/// Trashed workbooks of the spaces where the principal may restore them.
async fn list_trash(State(state): State<Shared>, Extension(p): Extension<Principal>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            let spaces: HashMap<String, crate::auth::SpaceRole> = state.auth.visible_spaces(&p)?.into_iter().map(|(s, r)| (s.id, r)).collect();
            Ok(state.trash()?.into_iter().filter(|w| spaces.get(&w.meta.space).is_some_and(|r| r.allows(Cap::DeleteWorkbook))).collect::<Vec<_>>())
        })
        .await?,
    ))
}

async fn restore_workbook(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    let (space, _) = allowed(&state, &p, &id, Cap::DeleteWorkbook).await?;
    Ok(Json(
        blocking(move || {
            let meta = state.restore(&id)?;
            state.auth.audit(&p, "workbook_restored", Some(&space.org_id), Some(&space.id), Some(&id), json!({}));
            Ok(meta)
        })
        .await?,
    ))
}

async fn purge_workbook(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    let (space, _) = allowed(&state, &p, &id, Cap::Purge).await?;
    blocking(move || {
        state.purge(Some(&id))?;
        state.auth.audit(&p, "workbook_purged", Some(&space.org_id), Some(&space.id), Some(&id), json!({}));
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Remove for good every trashed workbook the principal may purge.
async fn empty_trash(State(state): State<Shared>, Extension(p): Extension<Principal>) -> ApiResult<impl IntoResponse> {
    blocking(move || {
        let spaces: HashMap<String, crate::auth::Space> =
            state.auth.visible_spaces(&p)?.into_iter().filter(|(_, r)| r.allows(Cap::Purge)).map(|(s, _)| (s.id.clone(), s)).collect();
        for w in state.trash()? {
            if let Some(space) = spaces.get(&w.meta.space) {
                state.purge(Some(&w.meta.id))?;
                state.auth.audit(&p, "workbook_purged", Some(&space.org_id), Some(&space.id), Some(&w.meta.id), json!({}));
            }
        }
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ImportQuery {
    filename: String,
    table: Option<String>,
}

/// Stream the request body to disk, then let DuckDB ingest it.
async fn import_file(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Query(q): Query<ImportQuery>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<impl IntoResponse> {
    let (space, _) = allowed(&state, &p, &id, Cap::Edit).await?;
    // One of the space's bulk-job slots while this runs (7.07).
    let _slot = state.job_slot(&space.id).map_err(ApiError::from)?;
    let h = open(&state, &id).await?;
    let file_name = upload_name(&q.filename)?;
    let (scratch, path, size) = receive(&state, &id, &file_name, &headers, body).await?;
    let import_path = path.clone();
    let actor = p.actor();
    let result = h
        .run(
            http_job(Class::Bulk, &state),
            None,
            None,
            Work::Write(Box::new(move |wb| {
                let imported = wb.acting_as(&actor, |wb| wb.import_any(&import_path, q.table.as_deref()))?;
                Ok((imported, wb.revision()?))
            })),
        )
        .await;
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    state.controller.release_upload(size);
    let (imported, revision) = result?;
    state.auth.record(&p, "data_imported", &space, &id, json!({ "file": file_name, "bytes": size, "revision": revision }));
    state.quota.forget(&space.id);
    let h2 = h.clone();
    let _ = blocking(move || h2.refresh_summary()).await;
    let _ = h.events.send(ws::Event::TablesChanged { origin: 0, revision });
    Ok(Json(imported))
}

/// A scheduler job for an HTTP request (session 0, a request number of its own).
pub(crate) fn http_job(class: Class, state: &Shared) -> crate::state::Job {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    let timeout = if class == Class::Bulk { state.config.job_timeout } else { state.config.request_timeout };
    crate::state::Job { key: (0, N.fetch_add(1, Ordering::Relaxed)), class, deadline: std::time::Instant::now() + timeout }
}

fn upload_name(filename: &str) -> ApiResult<String> {
    std::path::Path::new(filename)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.starts_with('.'))
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "invalid filename"))
}

/// Stream a request body into its own scratch directory. Same-name uploads cannot
/// collide, the file keeps its name (DuckDB picks the reader from it), and a broken
/// upload leaves nothing behind. The size is checked against `--max-upload-bytes` before
/// reading (from `Content-Length`) and while streaming, and every byte is reserved from
/// the temporary-disk budget as it arrives. On success the caller owns the reservation.
async fn receive(state: &Shared, workbook: &str, file_name: &str, headers: &HeaderMap, body: Body) -> ApiResult<(PathBuf, PathBuf, u64)> {
    let max = state.config.max_upload_bytes;
    let declared = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    if let Some(n) = declared.filter(|&n| n > max) {
        return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, too_big(n, max)));
    }
    // The space's storage: what the file would add, before and while it arrives.
    let space = {
        let (s, w) = (state.clone(), workbook.to_string());
        blocking(move || s.workbook_space(&w)).await?
    };
    let quota = {
        let (s, sp) = (state.clone(), space.clone());
        blocking(move || s.limits(&sp)).await?.storage_bytes
    };
    let used = match quota {
        Some(_) => {
            let (s, sp) = (state.clone(), space.clone());
            Some(blocking(move || s.storage_used(&sp)).await?)
        }
        None => None,
    };
    if let Some(u) = used {
        crate::quota::check_storage(u, declared.unwrap_or(0), quota).map_err(ApiError::from)?;
    }
    let scratch = state.scratch_dir(".uploads", workbook)?;
    let path = scratch.join(file_name);
    let mut reserved = 0u64;
    let received = async {
        let mut file = tokio::fs::File::create(&path).await.map_err(io_error)?;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, format!("upload interrupted: {e}")))?;
            let len = chunk.len() as u64;
            if reserved + len > max {
                return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, too_big(reserved + len, max)));
            }
            if let Some(u) = used {
                crate::quota::check_storage(u, reserved + len, quota).map_err(ApiError::from)?;
            }
            if let Err(left) = state.controller.reserve_upload(len) {
                return Err(ApiError::new(
                    StatusCode::INSUFFICIENT_STORAGE,
                    format!("the server's space for uploads is full ({} MiB left); try again later or ask the administrator to raise --temp-budget", left >> 20),
                ));
            }
            reserved += len;
            crate::state::fault_at("upload_write", &path).map_err(|e| ApiError::new(StatusCode::INSUFFICIENT_STORAGE, format!("{e:#}")))?;
            file.write_all(&chunk).await.map_err(io_error)?;
        }
        file.flush().await.map_err(io_error)?;
        file.sync_all().await.map_err(io_error)?;
        Ok::<(), ApiError>(())
    }
    .await;
    match received {
        Ok(()) => Ok((scratch, path, reserved)),
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&scratch).await;
            state.controller.release_upload(reserved);
            Err(e)
        }
    }
}

fn too_big(n: u64, max: u64) -> String {
    format!("the file is too large ({} MiB; the limit is {} MiB)", n.div_ceil(1 << 20), max >> 20)
}

fn io_error(e: std::io::Error) -> ApiError {
    let status = if e.raw_os_error() == Some(28) { StatusCode::INSUFFICIENT_STORAGE } else { StatusCode::BAD_REQUEST };
    ApiError::new(status, format!("upload interrupted: {e}"))
}

#[derive(Deserialize)]
struct UploadQuery {
    filename: String,
}

/// Stage a file and preview it; nothing is imported until `/import`.
async fn stage_upload(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<impl IntoResponse> {
    let (space, _) = allowed(&state, &p, &id, Cap::Edit).await?;
    // One of the space's bulk-job slots while this runs (7.07).
    let _slot = state.job_slot(&space.id).map_err(ApiError::from)?;
    let h = open(&state, &id).await?;
    let file_name = upload_name(&q.filename)?;
    let (dir, path, size_bytes) = receive(&state, &id, &file_name, &headers, body).await?;
    let p = path.clone();
    let preview = h.run(http_job(Class::Bulk, &state), None, None, Work::Write(Box::new(move |wb| wb.preview_any(&p, &[])))).await;
    match preview {
        Ok(preview) => {
            let upload_id = state.add_upload(crate::state::Upload {
                workbook: id,
                path,
                dir,
                size_bytes,
                touched: std::time::Instant::now(),
            });
            Ok(Json(UploadCreated { upload_id, filename: file_name, size_bytes, preview }))
        }
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            state.controller.release_upload(size_bytes);
            Err(e.into())
        }
    }
}

async fn preview_upload(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path((id, upload)): Path<(String, String)>,
    Json(req): Json<ImportRequest>,
) -> ApiResult<impl IntoResponse> {
    let (space, _) = allowed(&state, &p, &id, Cap::Edit).await?;
    // One of the space's bulk-job slots while this runs (7.07).
    let _slot = state.job_slot(&space.id).map_err(ApiError::from)?;
    let h = open(&state, &id).await?;
    let u = state.upload(&id, &upload)?;
    let work = Work::Write(Box::new(move |wb: &mut alephgrid_core::Workbook| wb.preview_any(&u.path, &req.types)));
    Ok(Json(h.run(http_job(Class::Bulk, &state), None, None, work).await?))
}

/// Import a staged upload. On failure the upload is kept, so the user can change column
/// types and try again without uploading the file again.
async fn import_upload(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path((id, upload)): Path<(String, String)>,
    Json(req): Json<ImportRequest>,
) -> ApiResult<impl IntoResponse> {
    let (space, _) = allowed(&state, &p, &id, Cap::Edit).await?;
    // One of the space's bulk-job slots while this runs (7.07).
    let _slot = state.job_slot(&space.id).map_err(ApiError::from)?;
    let h = open(&state, &id).await?;
    let u = state.upload(&id, &upload)?;
    {
        let (st, sp, n) = (state.clone(), space.id.clone(), u.size_bytes);
        blocking(move || st.check_storage(&sp, n)).await?;
    }
    let h2 = h.clone();
    let job = req.job.clone();
    let actor = p.actor();
    let (file, bytes) = (u.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), u.size_bytes);
    let work = Work::Write(Box::new(move |wb: &mut alephgrid_core::Workbook| {
        *h2.job.lock().unwrap() = job;
        let out = wb.acting_as(&actor, |wb| wb.import_any_with(&u.path, req.table.as_deref(), &req.types)).and_then(|i| Ok((i, wb.revision()?)));
        *h2.job.lock().unwrap() = None;
        out
    }));
    let (imported, revision) = h.run(http_job(Class::Bulk, &state), None, None, work).await?;
    state.auth.record(&p, "data_imported", &space, &id, json!({ "file": file, "bytes": bytes, "revision": revision }));
    state.quota.forget(&space.id);
    state.remove_upload(&upload);
    let h3 = h.clone();
    let _ = blocking(move || h3.refresh_summary()).await;
    let _ = h.events.send(ws::Event::TablesChanged { origin: 0, revision });
    Ok(Json(imported))
}

async fn cancel_upload(State(state): State<Shared>,
    Extension(p): Extension<Principal>, Path((id, upload)): Path<(String, String)>) -> ApiResult<impl IntoResponse> {
    allowed(&state, &p, &id, Cap::Edit).await?;
    state.upload(&id, &upload)?;
    state.remove_upload(&upload);
    Ok(StatusCode::NO_CONTENT)
}

async fn job_status(State(state): State<Shared>,
    Extension(p): Extension<Principal>, Path((id, job)): Path<(String, String)>) -> ApiResult<impl IntoResponse> {
    allowed(&state, &p, &id, Cap::View).await?;
    let h = open(&state, &id).await?;
    let running = h.job.lock().unwrap().as_deref() == Some(job.as_str());
    let p = h.progress.query_progress();
    Ok(Json(if running {
        JobStatus { running, percentage: p.percentage(), rows_processed: p.rows_processed(), total_rows: p.total_rows_to_process() }
    } else {
        JobStatus { running, percentage: None, rows_processed: 0, total_rows: 0 }
    }))
}

/// Cancel a running job. Only the named job is interrupted, never someone else's work.
async fn cancel_job(State(state): State<Shared>,
    Extension(p): Extension<Principal>, Path((id, job)): Path<(String, String)>) -> ApiResult<impl IntoResponse> {
    allowed(&state, &p, &id, Cap::Edit).await?;
    let h = open(&state, &id).await?;
    let current = h.job.lock().unwrap();
    if current.as_deref() != Some(job.as_str()) {
        return Err(ApiError::new(StatusCode::CONFLICT, "that job is not running"));
    }
    h.interrupt.interrupt();
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct ExportQuery {
    /// JSON-encoded ViewSpec, so downloads respect the current sort and filters.
    spec: String,
    #[serde(default = "default_format")]
    format: ExportFormat,
}

fn default_format() -> ExportFormat {
    ExportFormat::Csv
}

async fn export(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> ApiResult<Response> {
    let (space, _) = allowed(&state, &p, &id, Cap::Export).await?;
    // One of the space's bulk-job slots while this runs (7.07).
    let _slot = state.job_slot(&space.id).map_err(ApiError::from)?;
    let spec: ViewSpec = serde_json::from_str(&q.spec).map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let h = open(&state, &id).await?;
    let file_name = format!("{}.{}", spec.table, q.format.extension());
    let table = spec.table.clone();
    let scratch = state.scratch_dir(".exports", &id)?;
    let path = scratch.join("export");
    let out = path.clone();
    let format = q.format;
    let written = h
        .run(
            http_job(Class::Bulk, &state),
            None,
            None,
            Work::Read(Box::new(move |r: &alephgrid_core::Reader| {
                let view = r.open_view(spec)?;
                let done = r.export_view(view.view_id, &out, format);
                r.close_view(view.view_id);
                done
            })),
        )
        .await
        .map_err(ApiError::from);
    let opened = match written {
        Ok(()) => tokio::fs::File::open(&path).await.map_err(|e| ApiError::from(anyhow::Error::from(e))),
        Err(e) => Err(e),
    };
    // The open handle keeps the bytes readable after unlinking.
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    let file = opened?;
    let len = file.metadata().await.map_err(anyhow::Error::from)?.len();
    state.auth.record(&p, "data_exported", &space, &id, json!({ "kind": "table", "table": table, "format": format.extension(), "bytes": len }));
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "application/octet-stream".parse().unwrap());
    headers.insert(header::CONTENT_LENGTH, len.to_string().parse().unwrap());
    headers.insert(header::CONTENT_DISPOSITION, attachment(&file_name));
    Ok((headers, Body::from_stream(ReaderStream::new(file))).into_response())
}

/// Stream a finished query export (`export_query`) once; the file is removed afterwards.
async fn download(State(state): State<Shared>,
    Extension(p): Extension<Principal>, Path((id, download)): Path<(String, String)>) -> ApiResult<Response> {
    let (space, _) = allowed(&state, &p, &id, Cap::Export).await?;
    let d = state
        .downloads
        .take(&id, &download)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "this export was already downloaded or has expired; export again"))?;
    let opened = tokio::fs::File::open(&d.path).await;
    // The open handle keeps the bytes readable after the directory is removed.
    let _ = tokio::fs::remove_dir_all(&d.dir).await;
    let file = opened.map_err(|e| ApiError::from(anyhow::Error::from(e)))?;
    let len = file.metadata().await.map_err(anyhow::Error::from)?.len();
    state.auth.record(&p, "export_downloaded", &space, &id, json!({ "download": download, "bytes": len }));
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "application/octet-stream".parse().unwrap());
    headers.insert(header::CONTENT_LENGTH, len.to_string().parse().unwrap());
    headers.insert(header::CONTENT_DISPOSITION, attachment(&d.file_name));
    Ok((headers, Body::from_stream(ReaderStream::new(file))).into_response())
}

/// Download all sheets of a workbook as one .xlsx file.
async fn export_xlsx(State(state): State<Shared>,
    Extension(p): Extension<Principal>, Path(id): Path<String>) -> ApiResult<Response> {
    let (space, _) = allowed(&state, &p, &id, Cap::Export).await?;
    // One of the space's bulk-job slots while this runs (7.07).
    let _slot = state.job_slot(&space.id).map_err(ApiError::from)?;
    let h = open(&state, &id).await?;
    let name = h.meta.lock().unwrap().name.replace(['"', '/', '\\', '\r', '\n'], "");
    let scratch = state.scratch_dir(".exports", &id)?;
    let path = scratch.join("sheets.xlsx");
    let out = path.clone();
    let written = h
        .run(http_job(Class::Bulk, &state), None, None, Work::Read(Box::new(move |r: &alephgrid_core::Reader| r.sheets().export_xlsx(&out).map(|_| ()))))
        .await
        .map_err(ApiError::from);
    let opened = match written {
        Ok(()) => tokio::fs::File::open(&path).await.map_err(|e| ApiError::from(anyhow::Error::from(e))),
        Err(e) => Err(e),
    };
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    let file = opened?;
    let len = file.metadata().await.map_err(anyhow::Error::from)?.len();
    state.auth.record(&p, "data_exported", &space, &id, json!({ "kind": "xlsx", "bytes": len }));
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".parse().unwrap(),
    );
    headers.insert(header::CONTENT_LENGTH, len.to_string().parse().unwrap());
    headers.insert(header::CONTENT_DISPOSITION, attachment(&format!("{name}.xlsx")));
    Ok((headers, Body::from_stream(ReaderStream::new(file))).into_response())
}

async fn websocket(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    let (space, role) = allowed(&state, &p, &id, Cap::View).await?;
    let handle = open(&state, &id).await?;
    Ok(upgrade
        .max_message_size(state.config.max_message_bytes)
        .on_upgrade(move |socket| ws::serve_socket(socket, state, handle, p, role, space)))
}

async fn static_asset(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let (path, file) = match Assets::get(path) {
        Some(f) if !path.is_empty() => (path, f),
        _ => match Assets::get("index.html") {
            Some(f) => ("index.html", f),
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    "Web UI not built. Run `npm install && npm run build` in web/, then rebuild.",
                )
                    .into_response()
            }
        },
    };
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    ([(header::CONTENT_TYPE, mime.as_ref().to_string()), (header::CACHE_CONTROL, cache.to_string())], file.data)
        .into_response()
}

/// `Content-Disposition: attachment` for any file name: an ASCII fallback plus the exact
/// UTF-8 name (RFC 6266 / 5987), so non-ASCII names never break the header.
pub fn attachment(name: &str) -> axum::http::HeaderValue {
    let ascii: String = name
        .chars()
        .map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"))
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static("attachment"))
}

/// Open (or find) a workbook off the async threads: starting an allocation can migrate files.
async fn open(state: &Shared, id: &str) -> ApiResult<std::sync::Arc<crate::state::WorkbookHandle>> {
    let (state, id) = (state.clone(), id.to_string());
    blocking(move || state.get(&id)).await
}

pub(crate) async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(ApiError::from)
}
