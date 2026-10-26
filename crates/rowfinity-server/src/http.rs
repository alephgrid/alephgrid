//! REST endpoints, auth and the embedded web UI.

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State, WebSocketUpgrade};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use futures_util::TryStreamExt;
use rowfinity_engine::{ColumnType, ExportFormat, ImportProblem, UploadPreview, ViewSpec};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio_util::io::{ReaderStream, StreamReader};

use crate::state::{Shared, WorkbookPatch};
use crate::ws;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct Assets;

/// An error reply: `{"error": message}`, plus `problem` when an import can say what to fix.
pub struct ApiError(StatusCode, String, Option<Box<ImportProblem>>);

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> ApiError {
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

type ApiResult<T> = Result<T, ApiError>;

pub fn router(state: Shared, web_dir: Option<PathBuf>) -> Router {
    let api = Router::new()
        .route("/api/workbooks", get(list_workbooks).post(create_workbook))
        .route("/api/workbooks/{id}", get(get_workbook).patch(update_workbook).delete(delete_workbook))
        .route("/api/workbooks/{id}/duplicate", post(duplicate_workbook))
        .route("/api/workbooks/{id}/export.xlsx", get(export_xlsx))
        .route("/api/trash", get(list_trash).delete(empty_trash))
        .route("/api/trash/{id}", axum::routing::delete(purge_workbook))
        .route("/api/trash/{id}/restore", post(restore_workbook))
        .route("/api/workspaces", get(list_workspaces).post(create_workspace))
        .route("/api/workspaces/{id}", patch(rename_workspace).delete(delete_workspace))
        .route(
            "/api/workbooks/{id}/import",
            post(import_file).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/workbooks/{id}/export", get(export))
        .route("/api/workbooks/{id}/uploads", post(stage_upload).layer(DefaultBodyLimit::disable()))
        .route("/api/workbooks/{id}/uploads/{upload}", axum::routing::delete(cancel_upload))
        .route("/api/workbooks/{id}/uploads/{upload}/preview", post(preview_upload))
        .route("/api/workbooks/{id}/uploads/{upload}/import", post(import_upload))
        .route("/api/workbooks/{id}/jobs/{job}", get(job_status).delete(cancel_job))
        .route("/ws/{id}", get(websocket))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));

    let app = Router::new().route("/api/config", get(config)).merge(api);
    let app = match web_dir {
        Some(dir) => app.fallback_service(
            tower_http::services::ServeDir::new(&dir)
                .fallback(tower_http::services::ServeFile::new(dir.join("index.html"))),
        ),
        None => app.fallback(static_asset),
    };
    app.with_state(state)
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(tower_http::trace::TraceLayer::new_for_http())
}

async fn require_token(State(state): State<Shared>, req: Request, next: Next) -> Response {
    let Some(expected) = &state.config.token else {
        return next.run(req).await;
    };
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let query = req.uri().query().and_then(|q| {
        q.split('&').find_map(|kv| kv.strip_prefix("token=").map(|v| v.to_string()))
    });
    if bearer.as_deref() == Some(expected.as_str()) || query.as_deref() == Some(expected.as_str()) {
        next.run(req).await
    } else {
        ApiError::new(StatusCode::UNAUTHORIZED, "missing or wrong access token").into_response()
    }
}

async fn config(State(state): State<Shared>) -> Json<serde_json::Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "auth_required": state.config.token.is_some(),
        "allow_sql": state.config.allow_sql,
        "sheet_copy_rows": state.config.sheet_copy_rows,
    }))
}

async fn list_workbooks(State(state): State<Shared>) -> ApiResult<impl IntoResponse> {
    Ok(Json(state.list()?))
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

#[derive(Deserialize)]
struct CreateBody {
    name: String,
    #[serde(default)]
    workspace: Option<String>,
}

async fn create_workbook(State(state): State<Shared>, Json(body): Json<CreateBody>) -> ApiResult<impl IntoResponse> {
    let name = if body.name.trim().is_empty() { "Untitled workbook" } else { body.name.as_str() };
    let h = state.create(name, body.workspace.filter(|w| !w.is_empty()))?;
    let meta = h.meta.lock().unwrap().clone();
    Ok(Json(meta))
}

async fn duplicate_workbook(State(state): State<Shared>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(blocking(move || state.duplicate(&id)).await?))
}

async fn list_workspaces(State(state): State<Shared>) -> ApiResult<impl IntoResponse> {
    Ok(Json(state.workspaces()?))
}

async fn create_workspace(State(state): State<Shared>, Json(body): Json<NameBody>) -> ApiResult<impl IntoResponse> {
    Ok(Json(state.create_workspace(&body.name)?))
}

async fn rename_workspace(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<NameBody>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(state.rename_workspace(&id, &body.name)?))
}

async fn delete_workspace(State(state): State<Shared>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    state.delete_workspace(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_workbook(State(state): State<Shared>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    let h = state.get(&id)?;
    let meta = h.meta.lock().unwrap().clone();
    let (tables, sheets) = blocking(move || h.with_engine(|wb| Ok((wb.list_tables()?, wb.sheets().list())))).await?;
    Ok(Json(json!({ "workbook": meta, "tables": tables, "sheets": sheets })))
}

async fn update_workbook(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<WorkbookPatch>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(state.update(&id, body)?))
}

async fn delete_workbook(State(state): State<Shared>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    blocking(move || state.delete(&id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_trash(State(state): State<Shared>) -> ApiResult<impl IntoResponse> {
    Ok(Json(state.trash()?))
}

async fn restore_workbook(State(state): State<Shared>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(blocking(move || state.restore(&id)).await?))
}

async fn purge_workbook(State(state): State<Shared>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    blocking(move || state.purge(Some(&id))).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn empty_trash(State(state): State<Shared>) -> ApiResult<impl IntoResponse> {
    blocking(move || state.purge(None)).await?;
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
    Path(id): Path<String>,
    Query(q): Query<ImportQuery>,
    body: Body,
) -> ApiResult<impl IntoResponse> {
    let h = state.get(&id)?;
    let file_name = upload_name(&q.filename)?;
    let (scratch, path, _) = receive(&state, &file_name, body).await?;

    let h2 = h.clone();
    let import_path = path.clone();
    let result = blocking(move || {
        let imported = h2.with_engine(|wb| wb.import_any(&import_path, q.table.as_deref()))?;
        h2.refresh_summary()?;
        Ok(imported)
    })
    .await;
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    let imported = result?;
    let _ = h.events.send(ws::Event::TablesChanged { origin: 0 });
    Ok(Json(imported))
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
/// upload leaves nothing behind.
async fn receive(state: &Shared, file_name: &str, body: Body) -> ApiResult<(PathBuf, PathBuf, u64)> {
    let scratch = state.scratch_dir(".uploads")?;
    let path = scratch.join(file_name);
    let received = async {
        let mut file = tokio::fs::File::create(&path).await?;
        let stream = body.into_data_stream().map_err(std::io::Error::other);
        let mut reader = StreamReader::new(stream);
        let n = tokio::io::copy(&mut reader, &mut file).await?;
        file.flush().await?;
        file.sync_all().await?;
        Ok::<u64, std::io::Error>(n)
    }
    .await;
    match received {
        Ok(n) => Ok((scratch, path, n)),
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&scratch).await;
            Err(ApiError::new(StatusCode::BAD_REQUEST, format!("upload interrupted: {e}")))
        }
    }
}

#[derive(Deserialize)]
struct UploadQuery {
    filename: String,
}

/// Stage a file and preview it; nothing is imported until `/import`.
async fn stage_upload(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<UploadQuery>,
    body: Body,
) -> ApiResult<impl IntoResponse> {
    let h = state.get(&id)?;
    let file_name = upload_name(&q.filename)?;
    let (dir, path, size_bytes) = receive(&state, &file_name, body).await?;
    let p = path.clone();
    let preview = blocking(move || h.with_engine(|wb| wb.preview_any(&p, &[]))).await;
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
            Err(e)
        }
    }
}

async fn preview_upload(
    State(state): State<Shared>,
    Path((id, upload)): Path<(String, String)>,
    Json(req): Json<ImportRequest>,
) -> ApiResult<impl IntoResponse> {
    let h = state.get(&id)?;
    let u = state.upload(&id, &upload)?;
    Ok(Json(blocking(move || h.with_engine(|wb| wb.preview_any(&u.path, &req.types))).await?))
}

/// Import a staged upload. On failure the upload is kept, so the user can change column
/// types and try again without uploading the file again.
async fn import_upload(
    State(state): State<Shared>,
    Path((id, upload)): Path<(String, String)>,
    Json(req): Json<ImportRequest>,
) -> ApiResult<impl IntoResponse> {
    let h = state.get(&id)?;
    let u = state.upload(&id, &upload)?;
    let h2 = h.clone();
    let job = req.job.clone();
    let result = blocking(move || {
        h2.with_engine(|wb| {
            *h2.job.lock().unwrap() = job;
            let out = wb.import_any_with(&u.path, req.table.as_deref(), &req.types);
            *h2.job.lock().unwrap() = None;
            out
        })
    })
    .await;
    let imported = result?;
    state.remove_upload(&upload);
    let h3 = h.clone();
    let _ = blocking(move || h3.refresh_summary()).await;
    let _ = h.events.send(ws::Event::TablesChanged { origin: 0 });
    Ok(Json(imported))
}

async fn cancel_upload(State(state): State<Shared>, Path((id, upload)): Path<(String, String)>) -> ApiResult<impl IntoResponse> {
    state.upload(&id, &upload)?;
    state.remove_upload(&upload);
    Ok(StatusCode::NO_CONTENT)
}

async fn job_status(State(state): State<Shared>, Path((id, job)): Path<(String, String)>) -> ApiResult<impl IntoResponse> {
    let h = state.get(&id)?;
    let running = h.job.lock().unwrap().as_deref() == Some(job.as_str());
    let p = h.progress.query_progress();
    Ok(Json(if running {
        JobStatus { running, percentage: p.percentage(), rows_processed: p.rows_processed(), total_rows: p.total_rows_to_process() }
    } else {
        JobStatus { running, percentage: None, rows_processed: 0, total_rows: 0 }
    }))
}

/// Cancel a running job. Only the named job is interrupted, never someone else's work.
async fn cancel_job(State(state): State<Shared>, Path((id, job)): Path<(String, String)>) -> ApiResult<impl IntoResponse> {
    let h = state.get(&id)?;
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
    Path(id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> ApiResult<Response> {
    let spec: ViewSpec = serde_json::from_str(&q.spec).map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let h = state.get(&id)?;
    let file_name = format!("{}.{}", spec.table, q.format.extension());
    let scratch = state.scratch_dir(".exports")?;
    let path = scratch.join("export");
    let p = path.clone();
    let format = q.format;
    let written = blocking(move || {
        h.with_engine(|wb| {
            let view = wb.open_view(spec)?;
            let out = wb.export_view(view.view_id, &p, format);
            wb.close_view(view.view_id);
            out
        })
    })
    .await;
    let opened = match written {
        Ok(()) => tokio::fs::File::open(&path).await.map_err(|e| ApiError::from(anyhow::Error::from(e))),
        Err(e) => Err(e),
    };
    // The open handle keeps the bytes readable after unlinking.
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    let file = opened?;
    let len = file.metadata().await.map_err(anyhow::Error::from)?.len();
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "application/octet-stream".parse().unwrap());
    headers.insert(header::CONTENT_LENGTH, len.to_string().parse().unwrap());
    headers.insert(header::CONTENT_DISPOSITION, attachment(&file_name));
    Ok((headers, Body::from_stream(ReaderStream::new(file))).into_response())
}

/// Download all sheets of a workbook as one .xlsx file.
async fn export_xlsx(State(state): State<Shared>, Path(id): Path<String>) -> ApiResult<Response> {
    let h = state.get(&id)?;
    let name = h.meta.lock().unwrap().name.replace(['"', '/', '\\', '\r', '\n'], "");
    let scratch = state.scratch_dir(".exports")?;
    let path = scratch.join("sheets.xlsx");
    let p = path.clone();
    let written = blocking(move || h.with_engine(|wb| wb.sheets().export_xlsx(&p))).await;
    let opened = match written {
        Ok(()) => tokio::fs::File::open(&path).await.map_err(|e| ApiError::from(anyhow::Error::from(e))),
        Err(e) => Err(e),
    };
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    let file = opened?;
    let len = file.metadata().await.map_err(anyhow::Error::from)?.len();
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
    Path(id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    let handle = state.get(&id)?;
    Ok(upgrade
        .max_message_size(64 << 20)
        .on_upgrade(move |socket| ws::serve_socket(socket, state, handle)))
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

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(ApiError::from)
}
