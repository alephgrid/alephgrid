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
use rowfinity_engine::{ExportFormat, ViewSpec};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio_util::io::{ReaderStream, StreamReader};

use crate::state::{Shared, WorkbookPatch};
use crate::ws;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct Assets;

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::BAD_REQUEST, format!("{e:#}"))
    }
}

type ApiResult<T> = Result<T, ApiError>;

pub fn router(state: Shared, web_dir: Option<PathBuf>) -> Router {
    let api = Router::new()
        .route("/api/workbooks", get(list_workbooks).post(create_workbook))
        .route("/api/workbooks/{id}", get(get_workbook).patch(update_workbook).delete(delete_workbook))
        .route("/api/workbooks/{id}/duplicate", post(duplicate_workbook))
        .route("/api/workbooks/{id}/export.xlsx", get(export_xlsx))
        .route("/api/workspaces", get(list_workspaces).post(create_workspace))
        .route("/api/workspaces/{id}", patch(rename_workspace).delete(delete_workspace))
        .route(
            "/api/workbooks/{id}/import",
            post(import_file).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/workbooks/{id}/export", get(export))
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
        ApiError(StatusCode::UNAUTHORIZED, "missing or wrong access token".into()).into_response()
    }
}

async fn config(State(state): State<Shared>) -> Json<serde_json::Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "auth_required": state.config.token.is_some(),
        "allow_sql": state.config.allow_sql,
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
    let (tables, sheets) = blocking(move || {
        let wb = h.engine.lock().unwrap();
        Ok((wb.list_tables()?, wb.sheets().list()))
    })
    .await?;
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
    state.delete(&id)?;
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
    let file_name = std::path::Path::new(&q.filename)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.starts_with('.'))
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "invalid filename".into()))?;
    let uploads = h.dir.join("uploads");
    tokio::fs::create_dir_all(&uploads).await.map_err(anyhow::Error::from)?;
    let path = uploads.join(&file_name);
    let mut file = tokio::fs::File::create(&path).await.map_err(anyhow::Error::from)?;
    let stream = body.into_data_stream().map_err(std::io::Error::other);
    let mut reader = StreamReader::new(stream);
    tokio::io::copy(&mut reader, &mut file).await.map_err(anyhow::Error::from)?;
    file.flush().await.map_err(anyhow::Error::from)?;
    drop(file);

    let h2 = h.clone();
    let import_path = path.clone();
    let result = blocking(move || {
        let imported = h2.engine.lock().unwrap().import_any(&import_path, q.table.as_deref())?;
        h2.refresh_summary()?;
        Ok(imported)
    })
    .await;
    let _ = tokio::fs::remove_file(&path).await;
    let imported = result?;
    let _ = h.events.send(ws::Event::TablesChanged { origin: 0 });
    Ok(Json(imported))
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
    let spec: ViewSpec = serde_json::from_str(&q.spec).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    let h = state.get(&id)?;
    let exports = h.dir.join("exports");
    std::fs::create_dir_all(&exports).map_err(anyhow::Error::from)?;
    let file_name = format!("{}.{}", spec.table, q.format.extension());
    let path = exports.join(format!("{}-{}", std::process::id(), file_name));
    let p = path.clone();
    let format = q.format;
    blocking(move || {
        let mut wb = h.engine.lock().unwrap();
        let view = wb.open_view(spec)?;
        let out = wb.export_view(view.view_id, &p, format);
        wb.close_view(view.view_id);
        out
    })
    .await?;
    let file = tokio::fs::File::open(&path).await.map_err(anyhow::Error::from)?;
    let len = file.metadata().await.map_err(anyhow::Error::from)?.len();
    // The open handle keeps the bytes readable after unlinking.
    let _ = tokio::fs::remove_file(&path).await;
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "application/octet-stream".parse().unwrap());
    headers.insert(header::CONTENT_LENGTH, len.to_string().parse().unwrap());
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{}\"", file_name.replace('"', "")).parse().unwrap(),
    );
    Ok((headers, Body::from_stream(ReaderStream::new(file))).into_response())
}

/// Download all sheets of a workbook as one .xlsx file.
async fn export_xlsx(State(state): State<Shared>, Path(id): Path<String>) -> ApiResult<Response> {
    let h = state.get(&id)?;
    let name = h.meta.lock().unwrap().name.replace(['"', '/', '\\'], "");
    let exports = h.dir.join("exports");
    std::fs::create_dir_all(&exports).map_err(anyhow::Error::from)?;
    let path = exports.join(format!("{}-sheets.xlsx", std::process::id()));
    let p = path.clone();
    blocking(move || h.engine.lock().unwrap().sheets().export_xlsx(&p)).await?;
    let file = tokio::fs::File::open(&path).await.map_err(anyhow::Error::from)?;
    let len = file.metadata().await.map_err(anyhow::Error::from)?.len();
    let _ = tokio::fs::remove_file(&path).await;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".parse().unwrap(),
    );
    headers.insert(header::CONTENT_LENGTH, len.to_string().parse().unwrap());
    headers.insert(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}.xlsx\"").parse().unwrap());
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

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(ApiError::from)
}
