//! `rowfinity` — run the big-data spreadsheet on your laptop or your own server.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use rowfinity_server::http;
use rowfinity_server::state::{AppState, Config};

#[derive(Parser, Debug)]
#[command(name = "rowfinity", version, about = "Open-source spreadsheet for billion-row data. Runs locally or self-hosted.")]
struct Cli {
    /// Files to open (CSV, TSV, Parquet, JSON/JSONL, XLSX). XLSX files become editable sheets. They are imported into a new workbook.
    files: Vec<PathBuf>,

    /// Where workbooks are stored.
    #[arg(long, env = "ROWFINITY_DATA_DIR", default_value = "./rowfinity-data")]
    data_dir: PathBuf,

    /// Interface to bind. Use 0.0.0.0 to serve your team (set --token too).
    #[arg(long, env = "ROWFINITY_HOST", default_value = "127.0.0.1")]
    host: String,

    #[arg(long, env = "ROWFINITY_PORT", default_value_t = 7878)]
    port: u16,

    /// Shared access token required by the API and WebSocket.
    #[arg(long, env = "ROWFINITY_TOKEN")]
    token: Option<String>,

    /// Create a demo workbook with this many synthetic flight rows.
    #[arg(long)]
    demo_rows: Option<u64>,

    /// Memory shared by all open workbooks' DuckDB instances, e.g. "8GB". Defaults to 80%
    /// of RAM (or of the container's memory limit). Bounds DuckDB's buffer managers, not
    /// the process's total memory.
    #[arg(long, alias = "memory-limit", env = "ROWFINITY_MEMORY_BUDGET", value_parser = parse_size)]
    memory_budget: Option<u64>,

    /// DuckDB worker threads shared by all open workbooks. Defaults to all cores.
    #[arg(long, env = "ROWFINITY_THREADS")]
    threads: Option<usize>,

    /// Temporary disk for DuckDB spill files and staged uploads (half each), e.g. "100GB".
    #[arg(long, env = "ROWFINITY_TEMP_BUDGET", value_parser = parse_size, default_value = "64GiB")]
    temp_budget: u64,

    /// Smallest memory share one open workbook may get; below it, idle workbooks are closed
    /// or new ones refused.
    #[arg(long, env = "ROWFINITY_MIN_WORKBOOK_MEMORY", value_parser = parse_size, default_value = "256MiB")]
    min_workbook_memory: u64,

    /// Seconds without sessions or work before a workbook is closed (its data stays).
    #[arg(long, env = "ROWFINITY_IDLE_TIMEOUT", default_value_t = 600)]
    idle_timeout: u64,

    /// Seconds before an unused sorted or filtered view's materialization is dropped.
    #[arg(long, env = "ROWFINITY_VIEW_TTL", default_value_t = 900)]
    view_ttl: u64,

    /// Largest file upload, e.g. "100GB".
    #[arg(long, env = "ROWFINITY_MAX_UPLOAD", value_parser = parse_size, default_value = "100GiB")]
    max_upload: u64,

    /// Largest WebSocket message a client may send (edits, pastes).
    #[arg(long, env = "ROWFINITY_MAX_MESSAGE", value_parser = parse_size, default_value = "16MiB")]
    max_message: u64,

    /// Seconds an interactive request (fetch, edit) may take, waiting included.
    #[arg(long, env = "ROWFINITY_REQUEST_TIMEOUT", default_value_t = 30)]
    request_timeout: u64,

    /// Seconds a bulk job (import, query, sort, copy, export) may take.
    #[arg(long, env = "ROWFINITY_JOB_TIMEOUT", default_value_t = 3600)]
    job_timeout: u64,

    /// Seconds running work may take to finish on shutdown before it is interrupted.
    #[arg(long, env = "ROWFINITY_SHUTDOWN_GRACE", default_value_t = 30)]
    shutdown_grace: u64,

    /// Disable the ad-hoc SQL console (recommended for shared servers you don't fully trust users on).
    #[arg(long, env = "ROWFINITY_DISABLE_SQL")]
    disable_sql: bool,

    /// Most rows copied from a table or query into a sheet.
    #[arg(long, env = "ROWFINITY_SHEET_COPY_ROWS", default_value_t = rowfinity_server::ws::SHEET_COPY_ROWS)]
    sheet_copy_rows: usize,

    /// Serve the web UI from this directory instead of the embedded build.
    #[arg(long, env = "ROWFINITY_WEB_DIR")]
    web_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "rowfinity=info,tower_http=warn".into()))
        .init();
    let cli = Cli::parse();

    if cli.host != "127.0.0.1" && cli.host != "localhost" && cli.token.is_none() {
        tracing::warn!("serving on {} without --token: anyone who can reach this port can read and edit your data", cli.host);
    }

    let mut config = Config::new(cli.data_dir.clone());
    config.sheet_copy_rows = cli.sheet_copy_rows.max(1);
    config.token = cli.token.clone();
    config.allow_sql = !cli.disable_sql;
    config.memory_budget = cli.memory_budget;
    config.threads = cli.threads;
    config.temp_budget = Some(cli.temp_budget);
    config.min_workbook_memory = cli.min_workbook_memory;
    config.idle_timeout = Duration::from_secs(cli.idle_timeout);
    config.view_ttl = Duration::from_secs(cli.view_ttl);
    config.shutdown_grace = Duration::from_secs(cli.shutdown_grace);
    config.max_upload_bytes = cli.max_upload;
    config.max_message_bytes = cli.max_message as usize;
    config.request_timeout = Duration::from_secs(cli.request_timeout.max(1));
    config.job_timeout = Duration::from_secs(cli.job_timeout.max(1));
    let state = AppState::new(config)?;
    let janitor = rowfinity_server::state::start_janitor(&state);

    let mut open_path = String::new();
    if !cli.files.is_empty() {
        let first = cli.files[0].file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let h = state.create(&first, None)?;
        for file in &cli.files {
            let path = file.canonicalize().with_context(|| format!("cannot open {}", file.display()))?;
            let started = std::time::Instant::now();
            let imported = h.with_engine(|wb| wb.import_any(&path, None))?;
            tracing::info!("imported {} in {:.1?}: {}", file.display(), started.elapsed(), serde_json::to_string(&imported)?);
        }
        h.refresh_summary()?;
        open_path = format!("w/{}", h.meta.lock().unwrap().id);
    }
    if let Some(rows) = cli.demo_rows {
        let h = state.create("Demo: US flights", None)?;
        let started = std::time::Instant::now();
        let info = h.with_engine(|wb| wb.create_demo_table(rows))?;
        tracing::info!("generated {} demo rows in {:.1?}", info.row_count, started.elapsed());
        h.refresh_summary()?;
        open_path = format!("w/{}", h.meta.lock().unwrap().id);
    }

    let app = http::router(state.clone(), cli.web_dir.clone());
    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse().context("invalid host/port")?;
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
    let shown_host = if cli.host == "0.0.0.0" { "localhost" } else { cli.host.as_str() };
    let token = cli.token.as_ref().map(|t| format!("?token={t}")).unwrap_or_default();
    println!("\n  Rowfinity is running → http://{shown_host}:{}/{open_path}{token}\n  Data: {}\n", cli.port, cli.data_dir.display());

    let stopping = state.clone();
    let (begun_tx, begun_rx) = tokio::sync::oneshot::channel::<()>();
    let grace = Duration::from_secs(cli.shutdown_grace) + Duration::from_secs(10);
    let serve = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // Stop admitting work and drain every workbook now: open WebSocket sessions end
            // when their workbook closes, which lets the server finish.
            stopping.begin_shutdown();
            let s = stopping.clone();
            tokio::task::spawn_blocking(move || s.checkpoint_all());
            tokio::spawn(async {
                shutdown_signal().await;
                tracing::warn!("second signal: exiting without waiting for running work (committed changes are safe)");
                std::process::exit(130);
            });
            let _ = begun_tx.send(());
        });
    // Connections that never close (a client holding its socket) must not keep the
    // process alive: once shutdown began, wait at most the grace period plus 10 s.
    tokio::select! {
        r = serve => r?,
        _ = async { let _ = begun_rx.await; tokio::time::sleep(grace).await } => {
            tracing::warn!("connections still open after the grace period; stopping anyway");
        }
    }
    janitor.abort();
    let s = state.clone();
    tokio::task::spawn_blocking(move || s.checkpoint_all()).await?;
    tracing::info!("stopped cleanly");
    Ok(())
}

fn parse_size(s: &str) -> Result<u64, String> {
    rowfinity_server::runtime::budget::parse_bytes(s)
}

/// Ctrl-C, or SIGTERM from `docker stop` / systemd.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutting down");
}
